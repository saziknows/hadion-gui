//! The command palette (Ctrl+K, Ctrl+Shift+P): one fuzzy search over
//! pages and actions, branches and tags, pull requests and issues, files of
//! HEAD, commits, workflow runs, tasks and repositories, ranked with
//! frecency (`Settings::ext.palette`).
//!
//! - [`commands`]: the command catalogue (pages, app, git and area actions)
//!   and what each runs.
//! - [`verbs`]: what can be done with an item and the app actions for it.
//! - [`sources`]: the items of the active repository, rebuilt only when
//!   their data changed (stamps).
//! - [`results`]: the rows for the current input (search, the Recent and
//!   Suggested groups of an empty input, an item's actions).
//! - [`view`]: the overlay.
//!
//! The search engine (matcher, ranking, frecency, prefixes, builders) is
//! `hadion_core::devops::palette`.

pub mod commands;
pub mod results;
pub mod sources;
pub mod verbs;
pub mod view;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use hadion_core::devops::palette::{Ranker, Use, files, frecency};
use hadion_core::{Head, Snapshot};
use serde::{Deserialize, Serialize};

use crate::devops::ctx::Ctx;
use crate::devops::global::{GlobalCx, GlobalView};
use crate::state::{Action, RepoId, RepoState, Settings};

/// `git ls-files` of one repository.
#[derive(Default)]
pub struct FileList {
    pub loading: bool,
    /// HEAD when the list was read (it is read again when HEAD moves).
    pub head: Option<String>,
    pub paths: Option<Result<Arc<Vec<String>>, String>>,
    pub at: Option<Instant>,
    pub ms: u64,
}

/// The item whose actions are listed instead of the search results.
#[derive(Clone, Debug)]
pub struct Drill {
    pub item: hadion_core::devops::palette::Item,
    /// Opened from the Recent group: offer "Remove from recent".
    pub recent: bool,
    /// The input to restore when going back.
    pub query: String,
    pub selected: usize,
}

#[derive(Default)]
pub struct State {
    pub open: bool,
    /// The input, prefix included (`>push`).
    pub query: String,
    /// Highlighted row (headers are skipped).
    pub selected: usize,
    pub drill: Option<Drill>,
    /// The input takes the focus and the caret goes to the end (opened,
    /// text set by a prefix chip, Tab or a script).
    pub focus: bool,
    pub caret_end: bool,
    /// Keep the highlighted row in view on the next frame.
    pub scroll_to_selected: bool,
    pub scroll_offset: f32,
    /// Script: run (or show the actions of) the highlighted row.
    pub run_requested: bool,
    pub actions_requested: bool,
    pub files: HashMap<RepoId, FileList>,
    pub cache: sources::Cache,
    pub results: results::Results,
    pub ranker: Ranker,
    /// The row to highlight again after going back from an item's actions.
    pub restore_selected: Option<usize>,
}

/// A use to remember for frecency.
#[derive(Clone, Debug)]
pub struct UseRecord {
    pub key: String,
    /// Working directory for repository items, empty for app-wide ones.
    pub repo: String,
    pub title: String,
    pub subtitle: String,
}

#[derive(Clone, Debug)]
pub enum Act {
    /// Open with this input (script `palette-query`).
    Query(String),
    /// Move the highlight (script `palette-down N` / `palette-up N`).
    Move(i64),
    /// Run the highlighted row (script `palette-run`).
    Run,
    /// List the highlighted row's actions (script `palette-actions`).
    ShowActions,
    Close,
    /// Remember a use for frecency.
    Used(UseRecord),
    /// Forget an item (every action of it) in the Recent group.
    Forget {
        key: String,
        repo: String,
    },
    ClearRecent,
    /// `git ls-files` of the active repository.
    LoadFiles {
        repo: RepoId,
        head: Option<String>,
    },
}

pub enum Msg {
    Files {
        repo: RepoId,
        head: Option<String>,
        ms: u64,
        result: Result<Vec<String>, String>,
    },
}

impl Msg {
    /// One line for the activity log (`Err` when the work failed).
    pub fn summary(&self) -> Result<String, String> {
        match self {
            Msg::Files { result, ms, .. } => match result {
                Ok(p) => Ok(format!(
                    "{} for the command palette in {}",
                    crate::format::plural(p.len(), "file", "files"),
                    crate::format::millis(*ms)
                )),
                Err(e) => Err(e.clone()),
            },
        }
    }
}

/// App-wide preferences (`Settings::ext`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Items used from the palette, with use counts and decayed scores
    /// (at most `frecency::MAX_USES`).
    pub recent: Vec<Use>,
}

/// What the palette reads about the app: the settings and the repositories.
#[derive(Clone, Copy)]
pub struct Cx<'a> {
    pub settings: &'a Settings,
    pub repos: &'a [RepoState],
    pub active: usize,
}

impl<'a> Cx<'a> {
    pub fn of_view(v: &'a GlobalView) -> Self {
        Self {
            settings: v.settings,
            repos: v.repos,
            active: v.active,
        }
    }

    pub fn repo(&self) -> Option<&'a RepoState> {
        self.repos.get(self.active)
    }

    pub fn snap(&self) -> Option<&'a Snapshot> {
        self.repo().and_then(|r| r.snapshot.as_ref())
    }

    /// The checked-out branch (none on a detached HEAD).
    pub fn head_branch(&self) -> Option<&'a str> {
        match &self.snap()?.head {
            Head::Branch { name, .. } => Some(name),
            _ => None,
        }
    }

    pub fn github(&self) -> Option<&'a (String, String)> {
        self.repo().and_then(|r| r.github_remote.as_ref())
    }

    /// The key frecency uses for items of the active repository.
    pub fn repo_key(&self) -> String {
        self.repo()
            .map(|r| r.workdir.display().to_string())
            .unwrap_or_default()
    }
}

pub fn apply(cx: &mut Ctx, st: &mut State, g: &mut GlobalCx, act: Act) {
    match act {
        Act::Query(q) => {
            if !st.open {
                open(st);
            }
            st.query = q;
            st.drill = None;
            st.selected = 0;
            st.focus = true;
            st.caret_end = true;
            st.scroll_to_selected = true;
        }
        Act::Move(n) => {
            st.selected = (st.selected as i64 + n).max(0) as usize;
            st.scroll_to_selected = true;
        }
        Act::Run => st.run_requested = true,
        Act::ShowActions => st.actions_requested = true,
        Act::Close => st.open = false,
        Act::Used(u) => {
            let now = crate::format::now_secs();
            let uses = &mut g.settings.ext.palette.recent;
            frecency::record(uses, &u.key, &u.repo, &u.title, &u.subtitle, now);
        }
        Act::Forget { key, repo } => {
            frecency::forget(&mut g.settings.ext.palette.recent, &key, &repo);
        }
        Act::ClearRecent => g.settings.ext.palette.recent.clear(),
        Act::LoadFiles { repo, head } => {
            // Ctx is built for the active repository; a list asked for by
            // another one waits until that one is active again.
            if g.active_repo().map(|r| r.id) != Some(repo) {
                return;
            }
            let f = st.files.entry(repo).or_default();
            if f.loading {
                return;
            }
            f.loading = true;
            cx.read("List files for the command palette", move |dir, log| {
                let t = Instant::now();
                let result = files::list(dir, &mut log.commands);
                Msg::Files {
                    repo,
                    head,
                    ms: t.elapsed().as_millis() as u64,
                    result,
                }
            });
        }
    }
}

pub fn on_msg(_cx: &mut Ctx, st: &mut State, _g: &mut GlobalCx, msg: Msg) {
    match msg {
        Msg::Files {
            repo,
            head,
            ms,
            result,
        } => {
            let f = st.files.entry(repo).or_default();
            f.loading = false;
            f.head = head;
            f.ms = ms;
            f.at = Some(Instant::now());
            f.paths = Some(result.map(Arc::new));
        }
    }
}

/// Every frame; keep it cheap.
pub fn tick(_cx: &mut Ctx, st: &mut State, g: &mut GlobalCx) {
    // Forget the file lists of closed repositories.
    if st.files.len() > g.repos.len() {
        st.files.retain(|id, _| g.repos.iter().any(|r| r.id == *id));
    }
}

/// `HADION_SCRIPT` commands this area understands: `palette` (built in,
/// toggles), `palette-query TEXT`, `palette-down N`, `palette-up N`,
/// `palette-run`, `palette-actions`, `palette-close`.
pub fn script(cmd: &str, arg: &str) -> Option<Vec<Action>> {
    let n = || arg.trim().parse::<i64>().unwrap_or(1);
    let act = match cmd {
        "palette-query" => Act::Query(arg.to_owned()),
        "palette-down" => Act::Move(n()),
        "palette-up" => Act::Move(-n()),
        "palette-run" => Act::Run,
        "palette-actions" => Act::ShowActions,
        "palette-close" => Act::Close,
        _ => return None,
    };
    Some(vec![act.into()])
}

pub fn is_open(st: &State) -> bool {
    st.open
}

pub fn toggle(st: &mut State) {
    if st.open {
        st.open = false;
    } else {
        open(st);
    }
}

fn open(st: &mut State) {
    st.open = true;
    st.query.clear();
    st.drill = None;
    st.selected = 0;
    st.focus = true;
    st.caret_end = true;
    st.scroll_offset = 0.0;
    st.scroll_to_selected = true;
    st.run_requested = false;
    st.actions_requested = false;
    // Recompute: the data may have changed while the palette was closed.
    st.results = results::Results::default();
    st.ranker.reset();
}

/// The palette overlay (drawn above everything while open).
pub fn show(ctx: &egui::Context, st: &mut State, v: &GlobalView, actions: &mut Vec<Action>) {
    view::show(ctx, st, v, actions);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_commands() {
        let one = |c: &str, a: &str| {
            let v = script(c, a).expect(c);
            assert_eq!(v.len(), 1);
            match v.into_iter().next() {
                Some(Action::DevOps(crate::devops::DevOpsAction::Palette(a))) => a,
                other => panic!("{other:?}"),
            }
        };
        assert!(matches!(one("palette-query", ">push"), Act::Query(q) if q == ">push"));
        assert!(matches!(one("palette-down", "3"), Act::Move(3)));
        assert!(matches!(one("palette-down", ""), Act::Move(1)));
        assert!(matches!(one("palette-up", "2"), Act::Move(-2)));
        assert!(matches!(one("palette-run", ""), Act::Run));
        assert!(matches!(one("palette-actions", ""), Act::ShowActions));
        assert!(script("palette-nope", "").is_none());
    }

    #[test]
    fn toggling_resets_the_input() {
        let mut st = State::default();
        toggle(&mut st);
        assert!(st.open && st.focus);
        st.query = "abc".into();
        st.selected = 4;
        toggle(&mut st);
        assert!(!st.open);
        toggle(&mut st);
        assert!(st.query.is_empty());
        assert_eq!(st.selected, 0);
    }

    #[test]
    fn file_summary() {
        let m = Msg::Files {
            repo: 1,
            head: None,
            ms: 12,
            result: Ok(vec!["a".into(), "b".into()]),
        };
        assert!(m.summary().unwrap().starts_with("2 files"));
        let m = Msg::Files {
            repo: 1,
            head: None,
            ms: 0,
            result: Err("not a git repository".into()),
        };
        assert_eq!(m.summary(), Err("not a git repository".into()));
    }
}
