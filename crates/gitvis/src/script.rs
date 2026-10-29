//! A tiny UI script for smoke tests and screenshots, read from the
//! `GITVIS_SCRIPT` environment variable:
//!
//! ```text
//! GITVIS_SCRIPT="wait 3; tab 0; head; open src/main.rs; wait 2; shot diff.png; split; shot split.png; quit"
//! ```
//!
//! Commands: `wait SECONDS`, `shot PATH` (PNG of the window, rendered by egui
//! itself, so it works on any virtual desktop), `tab N`, `head` (select the
//! HEAD commit), `wip` (select the working tree), `open PATH` (diff of a file
//! in the selected commit), `open-wip PATH` (unstaged diff), `split`,
//! `unified`, `select HUNK FROM TO` (diff line rows), `activity`, `reflog`,
//! `theme`, `github` / `graph` (centre page), `run N` (open the N-th
//! listed workflow run), `job N` (log of the N-th job), `verbose`,
//! `security` (exposure page), `scan`, `scan-history N`, `finding N`,
//! `reveal`, `sec-tab NAME`, `osint`, `unreachable`, `osv`, `dorks`,
//! `pr-refs`, `item N`, `insights`, `ins-tab NAME`, `analyse N`, `weight`,
//! `date-debt`, `pickaxe TEXT`, `grep TEXT`, `recover`, `ins-item N`,
//! `blame PATH`, `quit`. Anything else goes to the DevOps areas
//! (`crate::devops::script`): `home`, `review`, `prs`, `pr N`, `ship TAB`,
//! `govern`, `workbench`, … (see each area's `script`).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub enum Cmd {
    Wait(f32),
    Shot(PathBuf),
    Tab(usize),
    Head,
    Wip,
    Open(String),
    OpenWip(String),
    Split,
    Unified,
    Select(usize, usize, usize),
    Activity,
    Reflog,
    Theme,
    /// Toggle a sidebar section/folder, e.g. `collapse sec:remote`.
    Collapse(String),
    /// `git add PATH` through the normal operation path (fills the activity log).
    Stage(String),
    Undo,
    /// Open the GitHub page (Actions tab) / go back to the graph.
    Github,
    Graph,
    /// Set the run list's text filter.
    Filter(String),
    /// Set the run list's status chip: `all`, `failed`, `running`, `passed`.
    Status(String),
    /// Open the N-th run in the current run list.
    Run(usize),
    /// Show the log of the N-th job of the selected run.
    JobLog(usize),
    Verbose,
    /// Exposure page.
    Security,
    /// Scan the working tree for secrets.
    Scan,
    /// Scan the working tree and the newest N commits (0 = all).
    ScanHistory(usize),
    /// Select the N-th listed exposure finding.
    Finding(usize),
    /// Reveal the selected finding's secret.
    Reveal,
    /// Exposure page tab: `secrets`, `people`, `footprint`, `files`,
    /// `history`, `github`, `supply`.
    SecTab(String),
    /// Run the OSINT analysis.
    Osint,
    /// `git fsck --unreachable` plus blob scan.
    Unreachable,
    /// osv.dev lookup of the lockfile packages.
    Osv,
    /// GitHub code-search dorks.
    Dorks,
    /// Pull-request refs on origin.
    PrRefs,
    /// Select the N-th item of the current OSINT tab.
    Item(usize),
    /// Insights page (`insights`), its tab (`ins-tab health|hotspots|owners|
    /// branches|commits|releases|timeline|debt|ci|licences|search|recovery`).
    Insights,
    InsTab(String),
    /// Run the insights analysis with the newest N commits (0 = all).
    Analyse(usize),
    /// History weight walk.
    Weight,
    /// Date the debt markers with git blame.
    DateDebt,
    /// Pickaxe search for TEXT (`pickaxe TEXT`), message search (`grep TEXT`).
    Pickaxe(String),
    Grep(String),
    /// Find lost commits and stashes.
    Recover,
    /// Select the N-th item of the current insights tab.
    InsItem(usize),
    /// Blame the selected / given file on the Owners tab.
    Blame(String),
    /// A command of a DevOps area (`devops::script`), e.g. `review`,
    /// `pr N`, `ship releases`, `govern`, `tasks`.
    Ext(String, String),
    /// Resize the window to this inner size in points (`window 900 560`,
    /// the smallest the window allows).
    Window(f32, f32),
    /// Close the open dialog without confirming it (after a screenshot of
    /// a confirmation, so the session can go on).
    DialogCancel,
    Quit,
}

pub struct Script {
    cmds: VecDeque<Cmd>,
    wait_until: Option<Instant>,
}

impl Script {
    pub fn from_env() -> Option<Self> {
        let text = std::env::var("GITVIS_SCRIPT").ok()?;
        let cmds: VecDeque<Cmd> = text.split(';').filter_map(parse).collect();
        if cmds.is_empty() {
            return None;
        }
        eprintln!("[script] {} commands", cmds.len());
        Some(Self {
            cmds,
            wait_until: None,
        })
    }

    /// The next command whose turn has come, if any.
    pub fn next_ready(&mut self) -> Option<Cmd> {
        if let Some(t) = self.wait_until {
            if Instant::now() < t {
                return None;
            }
            self.wait_until = None;
        }
        let cmd = self.cmds.pop_front()?;
        match &cmd {
            Cmd::Wait(s) => {
                self.wait_until = Some(Instant::now() + Duration::from_secs_f32(*s));
                return self.next_ready();
            }
            // Give the frame a chance to render before the next command.
            Cmd::Shot(_) => self.wait_until = Some(Instant::now() + Duration::from_millis(700)),
            _ => self.wait_until = Some(Instant::now() + Duration::from_millis(150)),
        }
        Some(cmd)
    }
}

fn parse(s: &str) -> Option<Cmd> {
    let mut parts = s.trim().splitn(2, ' ');
    let name = parts.next()?.trim();
    let arg = parts.next().unwrap_or("").trim();
    let num = |a: &str| a.parse::<usize>().ok();
    let cmd = match name {
        "" => return None,
        "wait" => Cmd::Wait(arg.parse().ok()?),
        "shot" => Cmd::Shot(PathBuf::from(arg)),
        "tab" => Cmd::Tab(num(arg)?),
        "head" => Cmd::Head,
        "wip" => Cmd::Wip,
        "open" => Cmd::Open(arg.to_owned()),
        "open-wip" => Cmd::OpenWip(arg.to_owned()),
        "split" => Cmd::Split,
        "unified" => Cmd::Unified,
        "select" => {
            let mut n = arg.split_whitespace().filter_map(num);
            Cmd::Select(n.next()?, n.next()?, n.next()?)
        }
        "activity" => Cmd::Activity,
        "reflog" => Cmd::Reflog,
        "theme" => Cmd::Theme,
        "window" => {
            let mut n = arg.split_whitespace().map(|x| x.parse::<f32>().ok());
            Cmd::Window(n.next()??, n.next()??)
        }
        "collapse" => Cmd::Collapse(arg.to_owned()),
        "stage" => Cmd::Stage(arg.to_owned()),
        "undo" => Cmd::Undo,
        "github" => Cmd::Github,
        "graph" => Cmd::Graph,
        "filter" => Cmd::Filter(arg.to_owned()),
        "status" => Cmd::Status(arg.to_owned()),
        "run" => Cmd::Run(num(arg)?),
        "job" => Cmd::JobLog(num(arg)?),
        "verbose" => Cmd::Verbose,
        "security" => Cmd::Security,
        "scan" => Cmd::Scan,
        "scan-history" => Cmd::ScanHistory(num(arg).unwrap_or(2000)),
        "finding" => Cmd::Finding(num(arg)?),
        "reveal" => Cmd::Reveal,
        "sec-tab" => Cmd::SecTab(arg.to_owned()),
        "osint" => Cmd::Osint,
        "unreachable" => Cmd::Unreachable,
        "osv" => Cmd::Osv,
        "dorks" => Cmd::Dorks,
        "pr-refs" => Cmd::PrRefs,
        "item" => Cmd::Item(num(arg)?),
        "insights" => Cmd::Insights,
        "ins-tab" => Cmd::InsTab(arg.to_owned()),
        "analyse" | "analyze" => Cmd::Analyse(num(arg).unwrap_or(10_000)),
        "weight" => Cmd::Weight,
        "date-debt" => Cmd::DateDebt,
        "pickaxe" => Cmd::Pickaxe(arg.to_owned()),
        "grep" => Cmd::Grep(arg.to_owned()),
        "recover" => Cmd::Recover,
        "ins-item" => Cmd::InsItem(num(arg)?),
        "blame" => Cmd::Blame(arg.to_owned()),
        "dialog-cancel" => Cmd::DialogCancel,
        "quit" => Cmd::Quit,
        other => Cmd::Ext(other.to_owned(), arg.to_owned()),
    };
    Some(cmd)
}

/// Save a screenshot delivered by egui as a PNG.
pub fn save_png(path: &std::path::Path, image: &egui::ColorImage) {
    let [w, h] = image.size;
    match image::save_buffer(
        path,
        image.as_raw(),
        w as u32,
        h as u32,
        image::ColorType::Rgba8,
    ) {
        Ok(()) => eprintln!("[script] saved {}", path.display()),
        Err(e) => eprintln!("[script] could not save {}: {e}", path.display()),
    }
}
