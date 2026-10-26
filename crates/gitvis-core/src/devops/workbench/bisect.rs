//! Bisect: state detection (`BISECT_START` / `BISECT_LOG` in the git
//! directory), the log parsed into verdicts, the remaining-steps estimate
//! (`git rev-list --bisect-vars`), the output of a step, and the command
//! lines for start, good / bad / skip, run and reset.

use std::path::{Path, PathBuf};

use super::shell::find_sh;
use super::{git, git_probe};
use crate::devops::proc::ProcSpec;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Verdict {
    Good,
    Bad,
    Skip,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Good => "Good",
            Verdict::Bad => "Bad",
            Verdict::Skip => "Skip",
        }
    }
}

/// The words used for bad and good (`git bisect start --term-new=…`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Terms {
    pub bad: String,
    pub good: String,
}

impl Default for Terms {
    fn default() -> Self {
        Self {
            bad: "bad".to_owned(),
            good: "good".to_owned(),
        }
    }
}

impl Terms {
    /// `BISECT_TERMS`: the bad term, then the good term.
    pub fn parse(text: &str) -> Terms {
        let mut l = text.lines().map(str::trim).filter(|l| !l.is_empty());
        match (l.next(), l.next()) {
            (Some(b), Some(g)) => Terms {
                bad: b.to_owned(),
                good: g.to_owned(),
            },
            _ => Terms::default(),
        }
    }

    pub fn word(&self, v: Verdict) -> &str {
        match v {
            Verdict::Good => &self.good,
            Verdict::Bad => &self.bad,
            Verdict::Skip => "skip",
        }
    }
}

/// One tested commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    pub verdict: Verdict,
    pub sha: String,
    pub subject: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BisectLog {
    /// The `git bisect start …` line.
    pub start: Option<String>,
    pub entries: Vec<LogEntry>,
    /// `(sha, subject)` once found.
    pub first_bad: Option<(String, String)>,
    /// Only skipped commits are left; the culprit is one of these.
    pub candidates: Vec<(String, String)>,
}

/// `[sha] subject` → `(sha, subject)`.
fn bracketed(s: &str) -> Option<(String, String)> {
    let s = s.trim();
    let rest = s.strip_prefix('[')?;
    let end = rest.find(']')?;
    Some((rest[..end].to_owned(), rest[end + 1..].trim().to_owned()))
}

/// Parse `git bisect log`.
pub fn parse_log(text: &str, terms: &Terms) -> BisectLog {
    let mut log = BisectLog::default();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("git bisect start") {
            log.start = Some(line.to_owned());
            continue;
        }
        let Some(comment) = line.strip_prefix("# ") else {
            continue;
        };
        if let Some(rest) = comment
            .strip_prefix("first bad commit:")
            .or_else(|| {
                comment
                    .strip_prefix("first '")
                    .and_then(|r| r.split_once("' commit:").map(|(_, rest)| rest))
            })
            .or_else(|| comment.strip_prefix(&format!("first {} commit:", terms.bad)))
        {
            log.first_bad = bracketed(rest);
            continue;
        }
        if let Some(rest) = comment.strip_prefix("possible first bad commit:") {
            if let Some(c) = bracketed(rest) {
                log.candidates.push(c);
            }
            continue;
        }
        let Some((word, rest)) = comment.split_once(':') else {
            continue;
        };
        let verdict = if word == terms.bad {
            Verdict::Bad
        } else if word == terms.good {
            Verdict::Good
        } else if word == "skip" {
            Verdict::Skip
        } else {
            continue;
        };
        if let Some((sha, subject)) = bracketed(rest) {
            log.entries.push(LogEntry {
                verdict,
                sha,
                subject,
            });
        }
    }
    log
}

/// `git rev-list --bisect-vars` output.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Vars {
    /// The commit git would test next.
    pub rev: String,
    /// Revisions left after testing `rev`.
    pub left: usize,
    /// Every candidate (`bisect_all`).
    pub candidates: usize,
    /// Roughly how many steps after this one.
    pub steps: usize,
}

pub fn parse_vars(text: &str) -> Option<Vars> {
    let mut v = Vars::default();
    let mut any = false;
    for line in text.lines() {
        let Some((k, val)) = line.split_once('=') else {
            continue;
        };
        let val = val.trim().trim_matches('\'');
        match k.trim() {
            "bisect_rev" => v.rev = val.to_owned(),
            "bisect_nr" => v.left = val.parse().unwrap_or(0),
            "bisect_all" => v.candidates = val.parse().unwrap_or(0),
            "bisect_steps" => v.steps = val.parse().unwrap_or(0),
            _ => continue,
        }
        any = true;
    }
    any.then_some(v)
}

/// Steps a bisection of `n` candidates needs: ⌈log2 n⌉.
pub fn steps_for(n: usize) -> usize {
    if n <= 1 {
        0
    } else {
        (usize::BITS - (n - 1).leading_zeros()) as usize
    }
}

/// What `git bisect good|bad|skip|start` printed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepResult {
    /// Checked out the next commit to test.
    Next {
        left: usize,
        steps: usize,
        sha: String,
        subject: String,
    },
    FirstBad {
        sha: String,
    },
    /// Only skipped commits remain.
    OnlySkipped,
    /// Waiting for a good or a bad commit.
    Waiting,
    Other,
}

pub fn parse_step(text: &str) -> StepResult {
    let mut next: Option<(usize, usize)> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Bisecting: ") {
            // "9 revisions left to test after this (roughly 3 steps)"
            let left = rest
                .split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            let steps = rest
                .split("roughly ")
                .nth(1)
                .and_then(|r| r.split_whitespace().next())
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            next = Some((left, steps));
            continue;
        }
        if let Some((left, steps)) = next
            && let Some((sha, subject)) = bracketed(line)
        {
            return StepResult::Next {
                left,
                steps,
                sha,
                subject,
            };
        }
        if line.contains(" is the first ") && line.ends_with("commit") {
            let sha = line.split_whitespace().next().unwrap_or("").to_owned();
            return StepResult::FirstBad { sha };
        }
        if line.contains("only 'skip'ped commits left")
            || line.contains("only skipped commits left")
        {
            return StepResult::OnlySkipped;
        }
        if line.starts_with("status: waiting") || line.contains("waiting for") {
            return StepResult::Waiting;
        }
    }
    StepResult::Other
}

/// A commit shown on the bisect page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommitInfo {
    pub sha: String,
    pub short: String,
    pub subject: String,
    pub message: String,
    pub author: String,
    pub email: String,
    pub time: i64,
    /// `(path, added, deleted)`; `None` counts for binary files.
    pub files: Vec<(String, Option<u64>, Option<u64>)>,
}

/// Read one commit with its file stats.
pub fn commit_info(dir: &Path, rev: &str, cmds: &mut Vec<String>) -> Option<CommitInfo> {
    let text = git(
        dir,
        &[
            "-c",
            "core.quotepath=false",
            "show",
            "--numstat",
            "--format=%H%x1f%h%x1f%an%x1f%ae%x1f%at%x1f%B%x1d",
            rev,
        ],
        cmds,
    )
    .ok()?;
    parse_commit(&text)
}

pub(crate) fn parse_commit(text: &str) -> Option<CommitInfo> {
    let (head, stats) = text.split_once('\u{1d}')?;
    let f: Vec<&str> = head.splitn(6, '\u{1f}').collect();
    if f.len() < 6 {
        return None;
    }
    let message = f[5].trim().to_owned();
    let files = stats
        .lines()
        .filter_map(|l| {
            let mut p = l.splitn(3, '\t');
            let (a, d, path) = (p.next()?, p.next()?, p.next()?);
            Some((path.to_owned(), a.parse().ok(), d.parse().ok()))
        })
        .collect();
    Some(CommitInfo {
        sha: f[0].trim().to_owned(),
        short: f[1].to_owned(),
        subject: message.lines().next().unwrap_or("").to_owned(),
        message,
        author: f[2].to_owned(),
        email: f[3].to_owned(),
        time: f[4].parse().unwrap_or(0),
        files,
    })
}

/// Everything the page shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BisectInfo {
    pub active: bool,
    pub terms: Terms,
    /// Where `git bisect reset` returns to (`BISECT_START`).
    pub start_ref: Option<String>,
    pub log: BisectLog,
    /// HEAD: the commit being tested.
    pub current: Option<CommitInfo>,
    pub vars: Option<Vars>,
    pub first_bad: Option<CommitInfo>,
    pub has_bad: bool,
    pub good_count: usize,
    /// Defaults for the start form.
    pub latest_tag: Option<String>,
    pub head_short: Option<String>,
}

fn git_dir(dir: &Path, cmds: &mut Vec<String>) -> Option<PathBuf> {
    git_probe(dir, &["rev-parse", "--absolute-git-dir"], cmds).map(PathBuf::from)
}

/// Is a bisection running?
pub fn active(dir: &Path) -> bool {
    let mut cmds = Vec::new();
    git_dir(dir, &mut cmds).is_some_and(|g| g.join("BISECT_START").is_file())
}

/// Read the bisect state.
pub fn read(dir: &Path, cmds: &mut Vec<String>) -> Result<BisectInfo, String> {
    let gd = git_dir(dir, cmds).ok_or_else(|| "Not a git repository".to_owned())?;
    let mut info = BisectInfo {
        active: gd.join("BISECT_START").is_file() || gd.join("BISECT_LOG").is_file(),
        ..Default::default()
    };
    info.head_short = git_probe(dir, &["rev-parse", "--short", "HEAD"], cmds);
    if !info.active {
        info.latest_tag = git_probe(dir, &["describe", "--tags", "--abbrev=0"], cmds);
        return Ok(info);
    }
    info.terms = std::fs::read_to_string(gd.join("BISECT_TERMS"))
        .map(|t| Terms::parse(&t))
        .unwrap_or_default();
    info.start_ref = std::fs::read_to_string(gd.join("BISECT_START"))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    let text = git(dir, &["bisect", "log"], cmds)?;
    info.log = parse_log(&text, &info.terms);
    info.current = commit_info(dir, "HEAD", cmds);
    let refs = git(
        dir,
        &["for-each-ref", "--format=%(refname)", "refs/bisect/"],
        cmds,
    )
    .unwrap_or_default();
    let bad_ref = format!("refs/bisect/{}", info.terms.bad);
    let good_prefix = format!("refs/bisect/{}-", info.terms.good);
    let goods: Vec<String> = refs
        .lines()
        .filter(|r| r.starts_with(&good_prefix))
        .map(str::to_owned)
        .collect();
    info.has_bad = refs.lines().any(|r| r == bad_ref);
    info.good_count = goods.len();
    if info.has_bad && !goods.is_empty() && info.log.first_bad.is_none() {
        let mut args: Vec<String> = vec!["rev-list".into(), "--bisect-vars".into(), bad_ref];
        args.push("--not".into());
        args.extend(goods);
        if let Ok(text) = git(dir, &args, cmds) {
            info.vars = parse_vars(&text);
        }
    }
    if let Some((sha, _)) = &info.log.first_bad {
        info.first_bad = commit_info(dir, sha, cmds);
    }
    Ok(info)
}

/// `git bisect start BAD GOOD…`.
pub fn start_args(bad: &str, goods: &[String]) -> Vec<String> {
    let mut v = vec!["bisect".to_owned(), "start".to_owned()];
    let bad = bad.trim();
    v.push(if bad.is_empty() { "HEAD" } else { bad }.to_owned());
    v.extend(
        goods
            .iter()
            .map(|g| g.trim().to_owned())
            .filter(|g| !g.is_empty()),
    );
    v
}

/// `git bisect good|bad|skip [REV]` (the current commit when `rev` is
/// `None`).
pub fn mark_args(terms: &Terms, verdict: Verdict, rev: Option<&str>) -> Vec<String> {
    let mut v = vec!["bisect".to_owned(), terms.word(verdict).to_owned()];
    if let Some(r) = rev {
        v.push(r.to_owned());
    }
    v
}

/// How the test command is run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shell {
    /// `sh -c CMD` (Git for Windows' sh on Windows).
    Sh(PathBuf),
    /// `cmd /C CMD`.
    Cmd,
}

impl Shell {
    /// The default: sh when it can be found, else cmd on Windows.
    pub fn default_for_platform() -> Shell {
        match find_sh() {
            Some(p) => Shell::Sh(p),
            None => Shell::Cmd,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Shell::Sh(p) => format!("sh -c ({})", p.display()),
            Shell::Cmd => "cmd /C".to_owned(),
        }
    }
}

/// `git bisect run SHELL FLAG CMD`, streamed. The command's exit code
/// decides: 0 good, 125 skip, 1–127 bad, 128 and above stop the run.
///
/// For `cmd`, `target` is the script written by [`write_cmd_script`]:
/// git hands the command to its own sh, which re-quotes arguments for
/// native programs with `\"` (cmd does not understand that), so the
/// command travels in a file instead.
pub fn run_spec(dir: &Path, shell: &Shell, target: &str) -> ProcSpec {
    let spec = ProcSpec::new("git", dir).args(["bisect", "run"]);
    match shell {
        Shell::Sh(sh) => spec.args([
            sh.to_string_lossy().into_owned(),
            "-c".to_owned(),
            target.to_owned(),
        ]),
        // Without this, git's sh would turn `/C` into a path.
        Shell::Cmd => spec
            .args(["cmd", "/C", target])
            .env("MSYS_NO_PATHCONV", "1"),
    }
}

/// The batch file that runs `cmd` and hands back its exit code.
pub fn cmd_script_text(cmd: &str) -> String {
    format!("@echo off\r\n{}\r\nexit /b %ERRORLEVEL%\r\n", cmd.trim())
}

/// Write [`cmd_script_text`] to a temporary `.cmd` file.
pub fn write_cmd_script(cmd: &str) -> Result<PathBuf, String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path =
        std::env::temp_dir().join(format!("gitvis-bisect-{}-{nanos}.cmd", std::process::id()));
    std::fs::write(&path, cmd_script_text(cmd))
        .map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    Ok(path)
}

/// The first bad commit announced in `git bisect run` output.
pub fn first_bad_in(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let l = l.trim();
        (l.contains(" is the first ") && l.ends_with("commit"))
            .then(|| l.split_whitespace().next().unwrap_or("").to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "\
git bisect start 'HEAD' 'v1.0'
# bad: [9a3a531ef0eecaf106838235c17ea8ced2ea47e1] commit 20
# good: [e6936e13e3b9cc1577ddc312fea01d439373457a] commit 1
git bisect good cc70c425fe10d5443c76e6962157ab40fa743105
# good: [cc70c425fe10d5443c76e6962157ab40fa743105] commit 10
# skip: [d9c068686e466adcec0940702c4e2fa1ac88ebe6] commit 15
git bisect bad 016ecb2638d6d22b4350a92eaac0627e40c23fea
# bad: [016ecb2638d6d22b4350a92eaac0627e40c23fea] commit 12
# first 'bad' commit: [016ecb2638d6d22b4350a92eaac0627e40c23fea] commit 12
";

    #[test]
    fn parses_the_log() {
        let log = parse_log(LOG, &Terms::default());
        assert_eq!(log.start.as_deref(), Some("git bisect start 'HEAD' 'v1.0'"));
        let v: Vec<(Verdict, &str)> = log
            .entries
            .iter()
            .map(|e| (e.verdict, e.subject.as_str()))
            .collect();
        assert_eq!(
            v,
            vec![
                (Verdict::Bad, "commit 20"),
                (Verdict::Good, "commit 1"),
                (Verdict::Good, "commit 10"),
                (Verdict::Skip, "commit 15"),
                (Verdict::Bad, "commit 12"),
            ]
        );
        assert_eq!(
            log.first_bad,
            Some((
                "016ecb2638d6d22b4350a92eaac0627e40c23fea".into(),
                "commit 12".into()
            ))
        );
        // Older git words it without quotes.
        let old = parse_log("# first bad commit: [abc] fix\n", &Terms::default());
        assert_eq!(old.first_bad, Some(("abc".into(), "fix".into())));
    }

    #[test]
    fn custom_terms() {
        let t = Terms::parse("broken\nfixed\n");
        assert_eq!(t.bad, "broken");
        let log = parse_log("# broken: [a] one\n# fixed: [b] two\n", &t);
        assert_eq!(log.entries[0].verdict, Verdict::Bad);
        assert_eq!(log.entries[1].verdict, Verdict::Good);
        assert_eq!(
            mark_args(&t, Verdict::Good, None),
            vec!["bisect".to_owned(), "fixed".to_owned()]
        );
        assert_eq!(
            mark_args(&Terms::default(), Verdict::Skip, Some("abc")),
            vec!["bisect", "skip", "abc"]
        );
    }

    #[test]
    fn vars_and_steps() {
        let v = parse_vars(
            "bisect_rev='cc70c425'\nbisect_nr=9\nbisect_good=9\nbisect_bad=8\nbisect_all=19\nbisect_steps=3\n",
        )
        .unwrap();
        assert_eq!(
            v,
            Vars {
                rev: "cc70c425".into(),
                left: 9,
                candidates: 19,
                steps: 3
            }
        );
        assert!(parse_vars("").is_none());
        assert_eq!(steps_for(1), 0);
        assert_eq!(steps_for(2), 1);
        assert_eq!(steps_for(19), 5);
        assert_eq!(steps_for(1024), 10);
    }

    #[test]
    fn step_output() {
        assert_eq!(
            parse_step(
                "Bisecting: 4 revisions left to test after this (roughly 2 steps)\n[d9c06868] commit 5\n"
            ),
            StepResult::Next {
                left: 4,
                steps: 2,
                sha: "d9c06868".into(),
                subject: "commit 5".into()
            }
        );
        let found = "016ecb26 is the first 'bad' commit\ncommit 016ecb26\nAuthor: T\n";
        assert_eq!(
            parse_step(found),
            StepResult::FirstBad {
                sha: "016ecb26".into()
            }
        );
        assert_eq!(first_bad_in(found).as_deref(), Some("016ecb26"));
        assert_eq!(
            parse_step("abc is the first bad commit\n"),
            StepResult::FirstBad { sha: "abc".into() }
        );
        assert_eq!(
            parse_step("status: waiting for good commit(s), bad commit known"),
            StepResult::Waiting
        );
        assert_eq!(parse_step("whatever"), StepResult::Other);
    }

    #[test]
    fn command_lines() {
        assert_eq!(
            start_args(" ", &["v1.0".into(), "".into()]),
            vec!["bisect", "start", "HEAD", "v1.0"]
        );
        let sh = run_spec(
            Path::new("."),
            &Shell::Sh(PathBuf::from("/bin/sh")),
            "make test",
        );
        assert_eq!(sh.program, "git");
        assert_eq!(sh.args, vec!["bisect", "run", "/bin/sh", "-c", "make test"]);
        let cmd = run_spec(Path::new("."), &Shell::Cmd, "C:/t/x.cmd");
        assert_eq!(cmd.args, vec!["bisect", "run", "cmd", "/C", "C:/t/x.cmd"]);
        assert!(cmd.env.contains(&("MSYS_NO_PATHCONV".into(), "1".into())));
        assert_eq!(
            cmd_script_text("npm test "),
            "@echo off\r\nnpm test\r\nexit /b %ERRORLEVEL%\r\n"
        );
    }

    #[test]
    fn commit_parsing() {
        let c = parse_commit("abc\u{1f}ab\u{1f}Ann\u{1f}a@x\u{1f}42\u{1f}Fix it\n\nBody\n\u{1d}\n2\t1\tsrc/x.rs\n-\t-\timg.png\n")
            .unwrap();
        assert_eq!(c.subject, "Fix it");
        assert_eq!(c.message, "Fix it\n\nBody");
        assert_eq!(c.files.len(), 2);
        assert_eq!(c.files[0], ("src/x.rs".into(), Some(2), Some(1)));
        assert_eq!(c.files[1].1, None);
    }
}
