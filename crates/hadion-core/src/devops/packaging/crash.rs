//! Crash reports: the text a panic hook writes, with secrets removed, the
//! file names, and reading them back for the Crash reports section.
//!
//! Nothing here uploads anything. The report text goes through the exposure
//! scanner ([`crate::secrets::scan_line`]) before it is written, because a
//! panic message can carry data (a path with a token, a URL with
//! credentials, a line of a file).
//!
//! Functions used by the panic hook must not panic themselves (a panic
//! inside a panic hook aborts the process): no indexing that can fail, no
//! unwraps.

use std::path::{Path, PathBuf};

/// What the panic hook collected.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CrashInfo {
    /// `2026-10-27T21:14:03Z`.
    pub time: String,
    pub version: String,
    /// `051465f (dirty)`.
    pub commit: String,
    pub build_date: String,
    /// `x86_64-pc-windows-gnu (release)`.
    pub target: String,
    /// `Windows 11 (10.0.26300.6899), x86_64`.
    pub os: String,
    pub exe: String,
    /// `main`, or the thread's name, or `unnamed`.
    pub thread: String,
    pub message: String,
    /// `src/app.rs:120:9`.
    pub location: String,
    pub backtrace: String,
}

/// The first line of every report.
pub const HEADER: &str = "Hadion crash report";

/// The report as written to disk (before redaction).
pub fn format_report(c: &CrashInfo) -> String {
    let mut s = String::with_capacity(4096 + c.backtrace.len());
    s.push_str(HEADER);
    s.push('\n');
    s.push_str(&"=".repeat(HEADER.len()));
    s.push_str("\n\n");
    for (k, v) in [
        ("Time", &c.time),
        ("Version", &c.version),
        ("Commit", &c.commit),
        ("Built", &c.build_date),
        ("Target", &c.target),
        ("OS", &c.os),
        ("Executable", &c.exe),
        ("Thread", &c.thread),
        ("Location", &c.location),
    ] {
        s.push_str(&format!("{:<12}{}\n", format!("{k}:"), one_line(v)));
    }
    s.push_str("\nMessage\n-------\n");
    s.push_str(c.message.trim_end());
    s.push_str("\n\nBacktrace\n---------\n");
    let bt = trim_backtrace(&c.backtrace);
    s.push_str(if bt.trim().is_empty() {
        "(not captured)"
    } else {
        bt.trim_end()
    });
    s.push('\n');
    s
}

/// Frames of the panic machinery itself (the hook, `std::panicking`, the
/// backtrace capture): noise at the top of every report.
fn is_machinery(frame: &str) -> bool {
    const NOISE: [&str; 10] = [
        "std::backtrace",
        "std::panicking",
        "core::panicking",
        "rust_begin_unwind",
        "__rust_end_short_backtrace",
        "begin_panic",
        "PanicHookInfo",
        "packaging::crash::install",
        "packaging::crash::write_report",
        "core::result::unwrap_failed",
    ];
    NOISE.iter().any(|n| frame.contains(n))
}

/// Drop the panic machinery's frames from the top of a backtrace
/// (`  N: function` lines, each followed by `at file:line` lines), noting
/// how many were left out.
pub fn trim_backtrace(bt: &str) -> String {
    let is_head = |l: &str| {
        let t = l.trim_start();
        let digits = t.chars().take_while(char::is_ascii_digit).count();
        digits > 0 && t[digits..].starts_with(": ")
    };
    let lines: Vec<&str> = bt.lines().collect();
    let heads: Vec<usize> = (0..lines.len()).filter(|&i| is_head(lines[i])).collect();
    // The last machinery frame among the first 40.
    let Some(cut) = heads.iter().take(40).rposition(|&i| is_machinery(lines[i])) else {
        return bt.to_owned();
    };
    let Some(&start) = heads.get(cut + 1) else {
        return bt.to_owned();
    };
    let mut out = format!("   ({} frames of the panic handler left out)\n", cut + 1);
    for l in &lines[start..] {
        out.push_str(l);
        out.push('\n');
    }
    out
}

fn one_line(v: &str) -> String {
    let t = v.trim();
    if t.is_empty() {
        "unknown".to_owned()
    } else {
        t.replace(['\r', '\n'], " ")
    }
}

/// Replace every secret the exposure scanner finds with
/// `[redacted: <rule id>]`. Returns the text and the rule ids, one per
/// replacement.
pub fn redact(text: &str) -> (String, Vec<&'static str>) {
    let rules = crate::secrets::rules();
    let mut out = String::with_capacity(text.len());
    let mut found = Vec::new();
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let hits = crate::secrets::scan_line(rules, "crash-report.txt", line);
        if hits.is_empty() {
            out.push_str(line);
            continue;
        }
        let mut pos = 0usize;
        for h in &hits {
            // Hits are sorted and do not overlap; guard anyway.
            if h.start < pos || h.end > line.len() {
                continue;
            }
            let (Some(before), Some(id)) = (
                line.get(pos..h.start),
                rules.rules.get(h.rule).map(|r| r.id),
            ) else {
                continue;
            };
            out.push_str(before);
            out.push_str(&format!("[redacted: {id}]"));
            found.push(id);
            pos = h.end;
        }
        out.push_str(line.get(pos..).unwrap_or(""));
    }
    (out, found)
}

/// The final file contents: the redacted report plus a note on what was
/// removed.
pub fn finish(report: &str) -> String {
    let (mut text, found) = redact(report);
    if !found.is_empty() {
        let mut ids: Vec<&str> = found.clone();
        ids.sort_unstable();
        ids.dedup();
        let line = format!(
            "Redacted:   {} value{} that looked secret ({})\n",
            found.len(),
            if found.len() == 1 { "" } else { "s" },
            ids.join(", ")
        );
        // In the header (the list reads only the head of each file).
        match text.find("\nMessage\n") {
            Some(i) => text.insert_str(i, &line),
            None => text.push_str(&line),
        }
    }
    text
}

/// `crash-20261027-211403Z.txt`.
pub fn file_name(compact_stamp: &str) -> String {
    format!("crash-{compact_stamp}.txt")
}

pub fn is_report_name(name: &str) -> bool {
    name.starts_with("crash-") && name.ends_with(".txt")
}

/// Write a report into `dir` (created when missing) under a new name: the
/// stamp, then `-2`, `-3`, … when two crashes share a second.
pub fn write_new(dir: &Path, compact_stamp: &str, text: &str) -> std::io::Result<PathBuf> {
    use std::io::Write;
    std::fs::create_dir_all(dir)?;
    for n in 1..100u32 {
        let name = if n == 1 {
            file_name(compact_stamp)
        } else {
            format!("crash-{compact_stamp}-{n}.txt")
        };
        let path = dir.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                f.write_all(text.as_bytes())?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("too many reports in the same second"))
}

/// The header fields of a report, read back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub time: String,
    pub version: String,
    pub commit: String,
    pub thread: String,
    pub location: String,
    /// The first non-empty line of the message.
    pub headline: String,
    /// The `Redacted:` line, if any.
    pub redacted: String,
}

impl Summary {
    /// Did the main (UI) thread panic, i.e. did the window close?
    pub fn fatal(&self) -> bool {
        self.thread == "main" || self.thread.is_empty()
    }
}

/// Read the header of a report written by [`format_report`].
pub fn summarize(text: &str) -> Summary {
    let mut s = Summary::default();
    let mut lines = text.lines();
    for line in lines.by_ref() {
        if line.trim() == "Message" {
            break;
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim().to_owned();
        match k.trim() {
            "Time" => s.time = v,
            "Version" => s.version = v,
            "Commit" => s.commit = v,
            "Thread" => s.thread = v,
            "Location" => s.location = v,
            _ => {}
        }
    }
    for line in lines.by_ref() {
        let t = line.trim();
        if t.is_empty() || t.chars().all(|c| c == '-') {
            continue;
        }
        if t == "Backtrace" {
            break;
        }
        s.headline = t.chars().take(300).collect();
        break;
    }
    if let Some(r) = text.lines().find(|l| l.starts_with("Redacted:")) {
        s.redacted = r.trim_start_matches("Redacted:").trim().to_owned();
    }
    s
}

/// A report on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub summary: Summary,
}

/// Reports in `dir`, newest first (by name, which starts with the UTC
/// time). At most `limit`; a missing folder is an empty list.
pub fn list(dir: &Path, limit: usize) -> std::io::Result<Vec<Listed>> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut names: Vec<(String, PathBuf, u64)> = rd
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let meta = e.metadata().ok()?;
            (meta.is_file() && is_report_name(&name)).then(|| (name, e.path(), meta.len()))
        })
        .collect();
    names.sort_by(|a, b| b.0.cmp(&a.0));
    names.truncate(limit);
    Ok(names
        .into_iter()
        .map(|(name, path, bytes)| {
            let head = read_head(&path, 16 * 1024);
            Listed {
                name,
                path,
                bytes,
                summary: summarize(&head),
            }
        })
        .collect())
}

fn read_head(path: &Path, max: usize) -> String {
    use std::io::Read;
    let mut buf = Vec::new();
    if let Ok(f) = std::fs::File::open(path) {
        let _ = f.take(max as u64).read_to_end(&mut buf);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// The report for a GitHub issue: the home folder replaced by `~` (it
/// usually contains the user name). The text is already redacted.
pub fn for_issue(text: &str, home: Option<&str>) -> String {
    let mut t = text.replace("\r\n", "\n");
    if let Some(h) = home
        .map(|h| h.trim_end_matches(['/', '\\']))
        .filter(|h| h.len() > 3)
    {
        for variant in [h.to_owned(), h.replace('\\', "/"), h.replace('/', "\\")] {
            t = t.replace(&variant, "~");
        }
    }
    t
}

/// The title of a crash issue: `Crash: <headline>` (shortened).
pub fn issue_title(s: &Summary) -> String {
    let head: String = s.headline.chars().take(90).collect();
    let more = s.headline.chars().count() > 90;
    if head.is_empty() {
        "Crash report".to_owned()
    } else {
        format!("Crash: {head}{}", if more { "…" } else { "" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CrashInfo {
        CrashInfo {
            time: "2026-10-27T21:14:03Z".into(),
            version: "0.1.0".into(),
            commit: "051465f (dirty)".into(),
            build_date: "2026-10-27 20:58 UTC".into(),
            target: "x86_64-pc-windows-gnu (debug)".into(),
            os: "Windows 11 (10.0.26300), x86_64".into(),
            exe: "C:\\Users\\someone\\hadion\\hadion.exe".into(),
            thread: "main".into(),
            message: "index out of bounds: the len is 3 but the index is 7\nsecond line".into(),
            location: "crates/hadion/src/app.rs:120:9".into(),
            backtrace: "   0: std::backtrace::Backtrace::force_capture\n   1: hadion::main".into(),
        }
    }

    #[test]
    fn formats_and_reads_back() {
        let text = format_report(&sample());
        assert!(text.starts_with("Hadion crash report\n==================="));
        assert!(text.contains("Thread:     main\n"), "{text}");
        assert!(text.contains("Location:   crates/hadion/src/app.rs:120:9\n"));
        assert!(
            text.contains(
                "\nBacktrace\n---------\n   (1 frames of the panic handler left out)\n   1: hadion::main"
            ),
            "{text}"
        );
        let s = summarize(&text);
        assert_eq!(s.time, "2026-10-27T21:14:03Z");
        assert_eq!(s.version, "0.1.0");
        assert_eq!(s.commit, "051465f (dirty)");
        assert_eq!(s.location, "crates/hadion/src/app.rs:120:9");
        assert_eq!(
            s.headline,
            "index out of bounds: the len is 3 but the index is 7"
        );
        assert!(s.fatal());
        assert_eq!(
            issue_title(&s),
            "Crash: index out of bounds: the len is 3 but the index is 7"
        );
        // Missing values and an empty backtrace.
        let empty = format_report(&CrashInfo::default());
        assert!(empty.contains("Version:    unknown\n"));
        assert!(empty.contains("(not captured)"));
        assert_eq!(summarize(&empty).headline, "");
        assert_eq!(issue_title(&summarize(&empty)), "Crash report");
        let bg = summarize(&format_report(&CrashInfo {
            thread: "worker".into(),
            ..sample()
        }));
        assert!(!bg.fatal());
    }

    #[test]
    fn panic_machinery_frames_are_left_out() {
        let bt = "   0: std::backtrace_rs::backtrace::win64::trace\n             at /rustc/x/backtrace/win64.rs:85:14\n   1: <std::backtrace::Backtrace>::force_capture\n   2: hadion::devops::packaging::crash::write_report\n             at .\\crates\\hadion\\src\\devops\\packaging\\crash.rs:84:20\n   3: std::panicking::panic_with_hook\n   4: __rustc::rust_begin_unwind\n   5: core::panicking::panic_fmt\n   6: hadion::app::Hadion::apply\n             at .\\crates\\hadion\\src\\app.rs:120:9\n   7: main\n";
        let t = trim_backtrace(bt);
        assert!(
            t.starts_with(
                "   (6 frames of the panic handler left out)\n   6: hadion::app::Hadion::apply\n"
            ),
            "{t}"
        );
        assert!(t.ends_with("   7: main\n"));
        // Nothing to trim, or nothing after the machinery: unchanged.
        assert_eq!(trim_backtrace("   0: main\n"), "   0: main\n");
        assert_eq!(
            trim_backtrace("   0: core::panicking::panic_fmt\n"),
            "   0: core::panicking::panic_fmt\n"
        );
        assert_eq!(trim_backtrace("disabled backtrace"), "disabled backtrace");
        let r = format_report(&CrashInfo {
            backtrace: bt.into(),
            ..sample()
        });
        assert!(
            r.contains("left out)\n   6: hadion::app::Hadion::apply"),
            "{r}"
        );
    }

    #[test]
    fn secrets_are_redacted() {
        // Fake, but shaped like real credentials (the scanner's own test
        // values, split so this file is not flagged itself).
        let token = format!("ghp_{}", "z1Yx2Wv3Ut4Sr5Qp6On7Ml8Kj9Ih0Hg1Fe2Dc3Ba4");
        let aws = &format!("AKIA{}", "J6ZQMPTX3V7UBNRQ");
        let msg = format!(
            "called `Result::unwrap()` on an `Err` value: request to https://api.github.com failed with token {token}\naws {aws} here"
        );
        let text = finish(&format_report(&CrashInfo {
            message: msg,
            ..sample()
        }));
        assert!(!text.contains(&token), "{text}");
        assert!(!text.contains(aws), "{text}");
        assert!(text.contains("[redacted: "), "{text}");
        // The note is in the header, right before the message.
        assert!(
            text.contains("app.rs:120:9\nRedacted:   2 values"),
            "{text}"
        );
        assert!(text.contains("that looked secret (aws-access-key, github-token)\n\nMessage\n"));
        let s = summarize(&text);
        assert!(
            s.redacted.starts_with("2 values that looked secret"),
            "{}",
            s.redacted
        );
        // Clean text is untouched.
        let clean = format_report(&sample());
        assert_eq!(finish(&clean), clean);
        let (same, found) = redact("no secrets\nhere");
        assert_eq!(same, "no secrets\nhere");
        assert!(found.is_empty());
    }

    #[test]
    fn files_on_disk() {
        let dir = std::env::temp_dir().join(format!("hadion-crash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(list(&dir, 10).unwrap().is_empty());
        let a = write_new(&dir, "20261027-211403Z", &format_report(&sample())).unwrap();
        let b = write_new(&dir, "20261027-211403Z", "Hadion crash report\n").unwrap();
        let c = write_new(&dir, "20261028-080000Z", &format_report(&sample())).unwrap();
        assert_eq!(a.file_name().unwrap(), "crash-20261027-211403Z.txt");
        assert_eq!(b.file_name().unwrap(), "crash-20261027-211403Z-2.txt");
        std::fs::write(dir.join("notes.txt"), "not a report").unwrap();
        let l = list(&dir, 10).unwrap();
        let names: Vec<&str> = l.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "crash-20261028-080000Z.txt",
                "crash-20261027-211403Z.txt",
                "crash-20261027-211403Z-2.txt"
            ]
        );
        assert_eq!(l[0].path, c);
        assert_eq!(l[0].summary.thread, "main");
        assert_eq!(list(&dir, 1).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn issue_text_hides_the_home_folder() {
        let t = for_issue(
            "Executable: C:\\Users\\someone\\hadion\\hadion.exe\r\nat C:/Users/someone/src/x.rs",
            Some("C:\\Users\\someone"),
        );
        assert_eq!(t, "Executable: ~\\hadion\\hadion.exe\nat ~/src/x.rs");
        assert_eq!(for_issue("a", None), "a");
        assert_eq!(for_issue("/a/b", Some("/")), "/a/b");
    }
}
