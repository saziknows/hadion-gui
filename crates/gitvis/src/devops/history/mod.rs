//! History browsing: the Compare page (two revisions: commits, files, diff),
//! actions on several commits picked in the graph (compare, squash, reword,
//! drop, cherry-pick, create patches), graph filters (author, path, date),
//! the file tree at a commit, inline blame with "blame the previous
//! version", and signature verification (badges in the graph, details in
//! the commit panel).
//!
//! One file per part, each with its state, controller and view:
//! [`compare`] (Compare tab), [`browse`] (Files at a commit), [`blame`]
//! (Blame), [`filter`] (the graph's filter bar), [`picks`] (multi-select
//! and its panel), [`sigs`] (verification, graph badges, commit panel).
//! [`widgets`] holds the revision input, file rows and code views.

mod blame;
mod browse;
mod compare;
mod filter;
mod picks;
mod sigs;
mod widgets;

use std::ops::Range;
use std::path::PathBuf;

use gitvis_core::devops::history::compare::{Comparison, Mode as RangeMode};
use gitvis_core::devops::history::filter::{QuerySet, Spec};
use gitvis_core::devops::history::multi::{RebaseAction, Totals};
use gitvis_core::devops::history::signature::{Setup, Verified};
use gitvis_core::devops::history::tree::Tree;
use serde::{Deserialize, Serialize};

use crate::devops::console::{Console, ConsoleRun};
use crate::devops::ctx::Ctx;
use crate::state::{Action, Page, RepoState, Settings};
use crate::theme::pal;
use crate::views::kit;
use gitvis_core::{Commit, Oid};

use crate::devops::hooks::HookCx;

pub use filter::{filter_bar, toolbar_button, toolbar_label};
pub use picks::PickEvent;
pub use sigs::{badge_tip as signature_tip, paint_badge as paint_signature};
pub use widgets::empty_state;

/// The three views of the Compare page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub enum Mode {
    #[default]
    Compare,
    Files,
    Blame,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Compare => "Compare",
            Mode::Files => "Files at a commit",
            Mode::Blame => "Blame",
        }
    }

    pub fn from_name(s: &str) -> Option<Mode> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "compare" => Mode::Compare,
            "files" | "browse" | "tree" => Mode::Files,
            "blame" => Mode::Blame,
            _ => return None,
        })
    }
}

#[derive(Default)]
pub struct State {
    pub mode: Mode,
    pub compare: compare::Compare,
    pub browse: browse::Browse,
    pub blame: blame::BlameView,
    pub filter: filter::Filter,
    pub picks: picks::Picks,
    pub sigs: sigs::Sigs,
    /// Give the WIP panel's commit message the focus on its next frame
    /// (`commit-focus`); the panel only borrows the state, hence the cell.
    pub focus_commit: std::cell::Cell<bool>,
}

impl State {
    /// Picked in the graph (Ctrl / Shift+click).
    pub fn is_picked(&self, id: &Oid) -> bool {
        self.picks.set.contains(id)
    }

    pub fn has_picks(&self) -> bool {
        !self.picks.set.is_empty()
    }

    /// Commit `index` is outside the active filter.
    pub fn dimmed(&self, index: usize) -> bool {
        self.filter.active() && !self.filter.matches(index)
    }

    /// The verified signature of a commit, when known.
    pub fn signature(&self, id: &Oid) -> Option<&gitvis_core::devops::history::signature::Sig> {
        self.sigs.get(id)
    }

    /// The rows the last verification request covered.
    pub fn verified_rows(&self) -> &Range<usize> {
        &self.sigs.visible
    }
}

/// Which filter field a script sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Author,
    Path,
    Since,
    Until,
    Message,
}

#[derive(Clone, Debug)]
pub enum Act {
    SetMode(Mode),
    // ------------------------------------------------------------ Compare
    /// Fill the inputs (B `None` = the working tree), optionally select a
    /// file once loaded, run, and show the page.
    Compare {
        a: String,
        b: Option<String>,
        select: Option<String>,
    },
    /// "Compare with…": A is set, B gets the focus.
    CompareFrom(String),
    RunCompare,
    SwapCompare,
    SetCompareMode(RangeMode),
    SetCompareTab(compare::Tab),
    SelectCompareFile(usize),
    SelectCompareCommit(compare::Side, usize),
    // -------------------------------------------------------------- Files
    /// Show the files of `rev`, optionally with one file open.
    Browse {
        rev: String,
        path: Option<String>,
    },
    /// Load the revision typed on the page.
    BrowseRev,
    BrowseSelect(String),
    BrowseToggle(String),
    /// Bring a line (1-based) of the open file into view and mark it.
    BrowseLine(u32),
    /// `git show REV:PATH` into a temporary file, opened with its program.
    OpenVersion {
        rev: String,
        path: String,
    },
    // -------------------------------------------------------------- Blame
    Blame {
        rev: String,
        path: String,
        line: Option<u32>,
    },
    /// Re-blame at the parent of line `index`'s commit, keeping the line.
    BlamePrevious(usize),
    BlameBack,
    CancelBlame,
    BlameMark(Option<u32>),
    // ------------------------------------------------------------- Filter
    ToggleFilterBar,
    /// A field changed on the bar (author, dates: matched at once).
    FilterEdited,
    /// Run the path / message query (Enter, or the button).
    FilterApply,
    FilterClear,
    /// Scripts: set one field and apply.
    FilterSet(Field, String),
    /// Next (true) or previous match.
    FilterStep(bool),
    // -------------------------------------------------------------- Picks
    Pick(PickEvent),
    ClearPicks,
    /// Scripts: pick the first N commit rows.
    PickFirst(usize),
    MultiCompare,
    MultiCherryPick,
    MultiCherryPickRun(Vec<String>),
    /// Create patch files in a folder (`None`: ask for one).
    MultiPatches(Option<PathBuf>),
    MultiPatchesRun(PathBuf, Vec<String>),
    MultiRebase(RebaseAction),
    // --------------------------------------------------------- Signatures
    /// The graph shows these rows (verification follows them).
    VisibleRows(Range<usize>),
    /// Verify one commit in full (the commit panel).
    VerifyDetail(Oid),
    /// Verify the visible rows now (scripts, "Check again").
    VerifyVisible,
    /// Forget every result and verify again (after fixing the setup).
    RecheckSignatures,
    /// Type into the toolbar's commit search (scripts).
    GraphSearch(String),
    /// Focus the WIP panel's commit message.
    FocusCommitMessage,
}

pub enum Msg {
    Compared(u64, Box<Result<Comparison, String>>),
    CompareDiff(u64, String, Box<Result<widgets::DiffShow, String>>),
    Tree(u64, Box<Result<Tree, String>>),
    Content(u64, String, Result<browse::ContentShow, String>),
    VersionSaved(Result<PathBuf, String>),
    Blamed(u64, Box<Result<blame::BlameShow, String>>),
    FilterQueried(u64, Result<QuerySet, String>),
    Totals(u64, Result<Totals, String>),
    Patches(Result<PathBuf, String>),
    Signatures(Vec<Oid>, Result<Verified, String>),
    SignatureDetail(Oid, Result<Verified, String>, Box<Setup>),
}

impl Msg {
    /// One line for the activity log (`Err` when the work failed).
    pub fn summary(&self) -> Result<String, String> {
        match self {
            Msg::Compared(_, r) => r.as_ref().as_ref().map_err(Clone::clone).map(|c| {
                format!(
                    "{}: {} only in A, {} only in B, {} files, {} ms",
                    c.request.title(),
                    c.count_a,
                    c.count_b,
                    c.files.len(),
                    c.elapsed_ms
                )
            }),
            Msg::CompareDiff(_, p, r) => r
                .as_ref()
                .as_ref()
                .map_err(Clone::clone)
                .map(|d| format!("Diff of {p}: {} hunks", d.doc.diff.hunks.len())),
            Msg::Tree(_, r) => r
                .as_ref()
                .as_ref()
                .map_err(Clone::clone)
                .map(|t| format!("{} files at {} · {} ms", t.totals().1, t.rev, t.elapsed_ms)),
            Msg::Content(_, p, Ok(_)) => Ok(format!("Read {p}")),
            Msg::Content(_, _, Err(e)) => Err(e.clone()),
            Msg::VersionSaved(Ok(p)) => Ok(format!("Saved {}", p.display())),
            Msg::VersionSaved(Err(e)) => Err(e.clone()),
            Msg::Blamed(_, r) => r.as_ref().as_ref().map_err(Clone::clone).map(|b| {
                format!(
                    "Blame of {} at {}: {} lines from {} commits · {} ms",
                    b.blame.path,
                    b.blame.rev,
                    b.blame.lines.len(),
                    b.blame.commits.len(),
                    b.blame.elapsed_ms
                )
            }),
            Msg::FilterQueried(_, Ok(q)) => Ok(format!(
                "{} commits match · {} ms{}",
                q.ids.len(),
                q.elapsed_ms,
                if q.truncated { " · capped" } else { "" }
            )),
            Msg::FilterQueried(_, Err(e)) => Err(e.clone()),
            Msg::Totals(_, Ok(t)) => Ok(format!(
                "{} commits: {} files, +{} −{}",
                t.commits, t.files, t.added, t.deleted
            )),
            Msg::Totals(_, Err(e)) => Err(e.clone()),
            Msg::Patches(Ok(p)) => Ok(format!("Patches in {}", p.display())),
            Msg::Patches(Err(e)) => Err(e.clone()),
            Msg::Signatures(ids, Ok(v)) => Ok(format!(
                "Verified {} commits: {} signed",
                ids.len(),
                v.sigs
                    .iter()
                    .filter(|(_, s)| s.status
                        != gitvis_core::devops::history::signature::Status::Unsigned)
                    .count()
            )),
            Msg::Signatures(_, Err(e)) => Err(e.clone()),
            Msg::SignatureDetail(id, Ok(v), _) => Ok(format!(
                "Signature of {}: {}",
                gitvis_core::repo::short_id(*id),
                v.sigs
                    .first()
                    .map(|(_, s)| s.status.label())
                    .unwrap_or("not signed")
            )),
            Msg::SignatureDetail(_, Err(e), _) => Err(e.clone()),
        }
    }
}

/// Persisted per repository.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// The graph filter.
    pub filter: Spec,
    pub filter_open: bool,
    /// The last comparison's inputs.
    pub compare_a: String,
    pub compare_b: String,
    pub compare_worktree: bool,
    pub compare_mode: RangeMode,
}

impl State {
    pub fn prefs(&self) -> Prefs {
        Prefs {
            filter: self.filter.spec.clone(),
            filter_open: self.filter.open,
            compare_a: self.compare.a.clone(),
            compare_b: self.compare.b.clone(),
            compare_worktree: self.compare.worktree,
            compare_mode: self.compare.mode,
        }
    }

    pub fn apply_prefs(&mut self, p: &Prefs) {
        self.filter.spec = p.filter.clone();
        self.filter.open = p.filter_open || p.filter.is_active();
        self.compare.a = p.compare_a.clone();
        self.compare.b = p.compare_b.clone();
        self.compare.worktree = p.compare_worktree;
        self.compare.mode = p.compare_mode;
    }
}

fn show_page(cx: &mut Ctx, repo: &RepoState) {
    if repo.page != Page::Compare {
        cx.action(Action::ShowPage(Page::Compare));
    }
}

pub fn apply(cx: &mut Ctx, repo: &mut RepoState, act: Act) {
    match act {
        Act::SetMode(m) => {
            repo.devops.history.mode = m;
            show_page(cx, repo);
            mode_shown(cx, repo);
        }
        Act::Compare { .. }
        | Act::CompareFrom(_)
        | Act::RunCompare
        | Act::SwapCompare
        | Act::SetCompareMode(_)
        | Act::SetCompareTab(_)
        | Act::SelectCompareFile(_)
        | Act::SelectCompareCommit(..) => compare::apply(cx, repo, act),
        Act::Browse { .. }
        | Act::BrowseRev
        | Act::BrowseSelect(_)
        | Act::BrowseToggle(_)
        | Act::BrowseLine(_)
        | Act::OpenVersion { .. } => browse::apply(cx, repo, act),
        Act::Blame { .. }
        | Act::BlamePrevious(_)
        | Act::BlameBack
        | Act::CancelBlame
        | Act::BlameMark(_) => blame::apply(cx, repo, act),
        Act::ToggleFilterBar
        | Act::FilterEdited
        | Act::FilterApply
        | Act::FilterClear
        | Act::FilterSet(..)
        | Act::FilterStep(_) => filter::apply(cx, repo, act),
        Act::Pick(_)
        | Act::ClearPicks
        | Act::PickFirst(_)
        | Act::MultiCompare
        | Act::MultiCherryPick
        | Act::MultiCherryPickRun(_)
        | Act::MultiPatches(_)
        | Act::MultiPatchesRun(..)
        | Act::MultiRebase(_) => picks::apply(cx, repo, act),
        Act::VisibleRows(_)
        | Act::VerifyDetail(_)
        | Act::VerifyVisible
        | Act::RecheckSignatures => sigs::apply(cx, repo, act),
        Act::FocusCommitMessage => repo.devops.history.focus_commit.set(true),
        Act::GraphSearch(text) => {
            repo.search = text;
            if repo.page != Page::Graph {
                cx.action(Action::ShowPage(Page::Graph));
            }
            cx.action(Action::SearchChanged);
        }
    }
}

pub fn on_msg(cx: &mut Ctx, repo: &mut RepoState, msg: Msg) {
    match msg {
        Msg::Compared(..) | Msg::CompareDiff(..) => compare::on_msg(cx, repo, msg),
        Msg::Tree(..) | Msg::Content(..) | Msg::VersionSaved(_) => browse::on_msg(cx, repo, msg),
        Msg::Blamed(..) => blame::on_msg(cx, repo, msg),
        Msg::FilterQueried(..) => filter::on_msg(cx, repo, msg),
        Msg::Totals(..) | Msg::Patches(_) => picks::on_msg(cx, repo, msg),
        Msg::Signatures(..) | Msg::SignatureDetail(..) => sigs::on_msg(cx, repo, msg),
    }
}

/// A console run of this repository ended.
pub fn on_console_done(_cx: &mut Ctx, _repo: &mut RepoState, _run: &ConsoleRun) {}

/// `GITVIS_SCRIPT` commands this area understands: `compare A B` (B may be
/// `worktree`; also `hist-compare A B`, since the Actions area answers to
/// a bare `compare`), `compare-mode two|three`, `compare-tab
/// files|commits`, `compare-file N`, `compare-commit a|b N`, `hist-mode
/// compare|files|blame`, `pick-commits N`, `clear-picks`, `multi-action
/// compare|cherry-pick|patches|rebase|squash|reword|drop` (writes stop at
/// their confirmation), `filter-author TEXT`, `filter-path PATH`,
/// `filter-since YYYY-MM-DD`, `filter-until YYYY-MM-DD`, `filter-message
/// TEXT`, `filter-clear`, `filter-bar`, `filter-next`, `filter-prev`,
/// `browse SHA|head`, `browse-file PATH`, `browse-line N`, `blame-at SHA
/// PATH [LINE]`, `blame-previous N` (line N's commit), `verify`,
/// `verify-detail`. Testing aids for the History page and the shell:
/// `commit-msg TEXT` (`\n` for a new line), `commit-focus`, `graph-search
/// TEXT` (the toolbar's commit search), `dialog
/// stash|branch|branch-invalid|tag|rename|delete-branch|push|reset|confirm|
/// confirm-action|clone`, `toast info|success|error TEXT`.
pub fn script(cmd: &str, arg: &str) -> Option<Vec<Action>> {
    let cmd = cmd.strip_prefix("hist-").unwrap_or(cmd);
    let arg = arg.trim();
    let num = arg.parse::<usize>().ok();
    let one = |a: Act| Some(vec![a.into()]);
    match cmd {
        "compare" => {
            let mut it = arg.split_whitespace();
            let a = it.next()?.to_owned();
            let b = match it.next() {
                None => Some("HEAD".to_owned()),
                Some("worktree" | "working-tree" | "wip") => None,
                Some(b) => Some(b.to_owned()),
            };
            one(Act::Compare { a, b, select: None })
        }
        "compare-mode" => one(Act::SetCompareMode(match arg {
            "three" | "3" | "..." => RangeMode::ThreeDot,
            _ => RangeMode::TwoDot,
        })),
        "compare-tab" => one(Act::SetCompareTab(match arg {
            "commits" => compare::Tab::Commits,
            _ => compare::Tab::Files,
        })),
        "compare-file" => one(Act::SelectCompareFile(num?)),
        "compare-commit" => {
            let (side, n) = arg.split_once(' ')?;
            let side = if side.eq_ignore_ascii_case("a") {
                compare::Side::A
            } else {
                compare::Side::B
            };
            one(Act::SelectCompareCommit(side, n.trim().parse().ok()?))
        }
        "mode" => one(Act::SetMode(Mode::from_name(arg)?)),
        "pick-commits" => one(Act::PickFirst(num?)),
        "clear-picks" => one(Act::ClearPicks),
        "multi-action" => one(match arg {
            "compare" => Act::MultiCompare,
            "cherry-pick" | "cherrypick" => Act::MultiCherryPick,
            "patches" | "patch" => Act::MultiPatches(Some(
                gitvis_core::devops::history::multi::default_patch_dir("script"),
            )),
            "rebase" | "squash" => Act::MultiRebase(RebaseAction::Squash),
            other => Act::MultiRebase(RebaseAction::from_name(other)?),
        }),
        "filter-author" => one(Act::FilterSet(Field::Author, arg.to_owned())),
        "filter-path" => one(Act::FilterSet(Field::Path, arg.to_owned())),
        "filter-since" => one(Act::FilterSet(Field::Since, arg.to_owned())),
        "filter-until" => one(Act::FilterSet(Field::Until, arg.to_owned())),
        "filter-message" => one(Act::FilterSet(Field::Message, arg.to_owned())),
        "filter-clear" => one(Act::FilterClear),
        "filter-bar" => one(Act::ToggleFilterBar),
        "filter-next" => one(Act::FilterStep(true)),
        "filter-prev" => one(Act::FilterStep(false)),
        "browse" => one(Act::Browse {
            rev: if arg.is_empty() || arg.eq_ignore_ascii_case("head") {
                "HEAD".into()
            } else {
                arg.to_owned()
            },
            path: None,
        }),
        "browse-file" => one(Act::BrowseSelect(arg.to_owned())),
        "blame-at" => {
            let mut it = arg.split_whitespace();
            let rev = it.next()?;
            let path = it.next()?.to_owned();
            let line = it.next().and_then(|l| l.parse().ok());
            one(Act::Blame {
                rev: if rev.eq_ignore_ascii_case("head") {
                    "HEAD".into()
                } else {
                    rev.to_owned()
                },
                path,
                line,
            })
        }
        "blame-previous" => one(Act::BlamePrevious(num?.saturating_sub(1))),
        "verify" => one(Act::VerifyVisible),
        "browse-line" => one(Act::BrowseLine(arg.parse().ok()?)),
        // Testing aids for the History page and the shell around it.
        "commit-msg" => Some(vec![Action::AddToCommitMessage {
            text: arg.replace("\\n", "\n"),
            at_start: false,
        }]),
        "commit-focus" => one(Act::FocusCommitMessage),
        "graph-search" => one(Act::GraphSearch(arg.to_owned())),
        "dialog" => Some(vec![test_dialog(arg)?]),
        "toast" => {
            let (kind, text) = arg.split_once(' ').unwrap_or((arg, ""));
            let kind = match kind {
                "error" => crate::state::ToastKind::Error,
                "success" => crate::state::ToastKind::Success,
                _ => crate::state::ToastKind::Info,
            };
            Some(vec![Action::Toast(kind, text.replace("\\n", "\n"))])
        }
        _ => None,
    }
}

/// `dialog NAME` (scripts): open a dialog with sample values to look at it.
/// Scripts never confirm a dialog; the operations are harmless anyway
/// (`confirm` runs nothing).
fn test_dialog(name: &str) -> Option<Action> {
    use crate::state::Dialog;
    use gitvis_core::ops::ResetMode;
    let s = |v: &str| v.to_owned();
    let d = match name.trim() {
        "stash" => Dialog::Stash {
            message: String::new(),
            include_untracked: true,
        },
        "branch" => Dialog::CreateBranch {
            name: String::new(),
            start: s("HEAD"),
            start_label: s("main"),
            checkout: true,
        },
        "branch-invalid" => Dialog::CreateBranch {
            name: s("bad name"),
            start: s("HEAD"),
            start_label: s("main"),
            checkout: true,
        },
        "tag" => Dialog::CreateTag {
            name: String::new(),
            target: s("HEAD"),
            target_label: s("8d2d4ba Speed up startup"),
            message: String::new(),
        },
        "rename" => Dialog::RenameBranch {
            from: s("feature/ui/details"),
            to: s("feature/ui/details"),
        },
        "delete-branch" => Dialog::DeleteBranch {
            name: s("feature/ui/details"),
            force: false,
            remote: Some((s("origin"), s("feature/ui/details"))),
            delete_remote: false,
        },
        "push" => Dialog::Push {
            branch: s("main"),
            remotes: vec![s("origin"), s("upstream")],
            remote: 0,
            set_upstream: true,
            force: true,
        },
        "reset" => Dialog::Reset {
            branch: s("main"),
            target: s("HEAD~1"),
            target_label: s("3ba97ba Describe worker threads"),
            mode: ResetMode::Hard,
        },
        "confirm" => Dialog::Confirm {
            title: s("Commit possible secrets?"),
            body: format!(
                "The staged changes contain 9 possible secrets:\n\n{}\n… 1 more\n\nOnce committed they stay in history even after you delete them; once pushed, everyone who can read the repository has them. Unstage the files or move the values to environment variables first.",
                (1..=8)
                    .map(|i| format!("• AWS access key · config/settings-{i}.json:{} · AKIA…{i}XYZ", 10 * i))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
            confirm: s("Commit anyway"),
            danger: true,
            ops: Vec::new(),
        },
        "confirm-action" => Dialog::ConfirmAction {
            title: s("Create the release v1.4.0?"),
            body: s("GitHub publishes the release and notifies everyone who watches the repository."),
            confirm: s("Create release"),
            danger: false,
            details: (1..=30)
                .map(|i| format!("POST /repos/owner/name/releases  {{\"tag_name\": \"v1.4.0\", \"line\": {i}, \"body\": \"a long request body that wraps across the dialog's width to check the scroll\"}}"))
                .collect(),
            action: Box::new(Action::Toast(crate::state::ToastKind::Info, s("confirmed"))),
        },
        "clone" => return Some(Action::CloneDialog),
        _ => return None,
    };
    Some(Action::Dialog(d))
}

/// After every load: drop picks that are gone, re-match the filter,
/// re-run its git query when the refs moved.
pub fn on_snapshot(cx: &mut Ctx, repo: &mut RepoState) {
    picks::on_snapshot(cx, repo);
    filter::on_snapshot(cx, repo);
}

/// The Compare page was opened.
pub fn on_page_shown(cx: &mut Ctx, repo: &mut RepoState) {
    mode_shown(cx, repo);
}

fn mode_shown(cx: &mut Ctx, repo: &mut RepoState) {
    match repo.devops.history.mode {
        Mode::Compare => {}
        Mode::Files => browse::shown(cx, repo),
        Mode::Blame => {}
    }
}

/// The Compare page (`Page::Compare`).
pub fn show_compare(
    ui: &mut egui::Ui,
    repo: &mut RepoState,
    _console: &Console,
    settings: &Settings,
    actions: &mut Vec<Action>,
) {
    let mode = repo.devops.history.mode;
    let subtitle = match mode {
        Mode::Compare => {
            "Two branches, tags or commits: the commits only on each side, the files that differ and their diff"
        }
        Mode::Files => "Every file of a commit as a folder tree, with sizes and the content",
        Mode::Blame => {
            "Who last changed each line of a file at any revision, and when; step back to the version before"
        }
    };
    page_header(ui, mode.label(), subtitle, |ui| {
        if let Some(m) = kit::segmented(
            ui,
            &[
                (Mode::Blame, Mode::Blame.label()),
                (Mode::Files, Mode::Files.label()),
                (Mode::Compare, Mode::Compare.label()),
            ],
            mode,
        ) {
            actions.push(Act::SetMode(m).into());
        }
    });
    if repo.snapshot.is_none() {
        let p = *pal(ui);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(p.canvas))
            .show(ui, |ui| kit::loading(ui, "Loading the repository…"));
        return;
    }
    match mode {
        Mode::Compare => compare::show(ui, repo, settings, actions),
        Mode::Files => browse::show(ui, repo, settings, actions),
        Mode::Blame => blame::show(ui, repo, settings, actions),
    }
}

/// `kit::page_header` with the actions placed first: the title and the
/// subtitle take the width the actions leave and the subtitle wraps there,
/// instead of running under the actions in a narrow window.
fn page_header(
    ui: &mut egui::Ui,
    title: &str,
    subtitle: &str,
    actions_ui: impl FnOnce(&mut egui::Ui),
) {
    use egui::{Align, Label, Layout, Margin, RichText};
    let p = *pal(ui);
    egui::Frame::NONE
        .inner_margin(Margin {
            left: 24,
            right: 16,
            top: 14,
            bottom: 10,
        })
        .show(ui, |ui| {
            // Top-aligned: a centred right-to-left row would fill the whole
            // page height (egui centres in the available height).
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                actions_ui(ui);
                ui.add_space(16.0);
                ui.with_layout(Layout::top_down(Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(
                        RichText::new(title)
                            .size(crate::theme::size::HEADING)
                            .family(crate::theme::semibold())
                            .color(p.text_strong),
                    );
                    if !subtitle.is_empty() {
                        ui.add(
                            Label::new(
                                RichText::new(subtitle)
                                    .size(crate::theme::size::SMALL)
                                    .color(p.text_2),
                            )
                            .wrap(),
                        );
                    }
                });
            });
        });
    kit::hairline(ui);
}

/// Branch, tag and remote names for the revision inputs, HEAD first.
fn ref_suggestions(repo: &RepoState) -> Vec<String> {
    let Some(snap) = repo.snapshot.as_ref() else {
        return Vec::new();
    };
    let mut v = vec!["HEAD".to_owned()];
    if let Some(b) = snap.current_branch() {
        v.push(b.name.clone());
        if let Some(u) = &b.upstream {
            v.push(u.clone());
        }
    }
    v.extend(snap.branches.iter().map(|b| b.name.clone()));
    v.extend(
        snap.remotes
            .iter()
            .flat_map(|r| r.branches.iter().map(|b| b.name.clone())),
    );
    v.extend(snap.tags.iter().rev().map(|t| t.name.clone()));
    v.push("HEAD~1".into());
    let mut seen = std::collections::HashSet::new();
    v.retain(|s| seen.insert(s.clone()));
    v
}

// ---------------------------------------------------------------------------
// Hooks
// ---------------------------------------------------------------------------

pub fn hook_commit_menu(ui: &mut egui::Ui, h: &HookCx, c: &Commit, actions: &mut Vec<Action>) {
    use crate::views::menus;
    let sha = c.id.to_string();
    menus::separator(ui);
    let is_head = h.snap.head.target() == Some(c.id);
    if !is_head {
        menus::action(
            ui,
            "Compare with HEAD",
            actions,
            Act::Compare {
                a: sha.clone(),
                b: Some("HEAD".into()),
                select: None,
            }
            .into(),
        );
    }
    menus::action(
        ui,
        "Compare with the working tree",
        actions,
        Act::Compare {
            a: sha.clone(),
            b: None,
            select: None,
        }
        .into(),
    );
    menus::action(
        ui,
        "Compare with…",
        actions,
        Act::CompareFrom(sha.clone()).into(),
    );
    // Branches and tags on this commit (their own menus have no hook yet).
    if let Some(labels) = h.snap.refs.get(&c.id) {
        let head_branch = h.snap.current_branch().map(|b| b.name.clone());
        for l in labels.iter().filter(|l| !l.is_head).take(3) {
            if let Some(hb) = &head_branch {
                menus::action(
                    ui,
                    &format!("Compare {hb} with {}", l.name),
                    actions,
                    Act::Compare {
                        a: l.name.clone(),
                        b: Some(hb.clone()),
                        select: None,
                    }
                    .into(),
                );
            }
        }
    }
    menus::action(
        ui,
        "Browse files at this commit",
        actions,
        Act::Browse {
            rev: sha,
            path: None,
        }
        .into(),
    );
    if !h.devops.history.picks.set.is_empty() {
        menus::action(
            ui,
            &format!(
                "Clear the {} picked commits",
                h.devops.history.picks.set.len()
            ),
            actions,
            Act::ClearPicks.into(),
        );
    }
}

pub fn hook_commit_panel(ui: &mut egui::Ui, h: &HookCx, c: &Commit, actions: &mut Vec<Action>) {
    sigs::commit_panel(ui, h, c, actions);
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        use crate::views::icons::{Icon, IconButton};
        if IconButton::new(Icon::Folder, "Browse files")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(
                Act::Browse {
                    rev: c.id.to_string(),
                    path: None,
                }
                .into(),
            );
        }
        if IconButton::new(Icon::Merge, "Compare with…")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Act::CompareFrom(c.id.to_string()).into());
        }
    });
}

pub fn hook_commit_file_menu(
    ui: &mut egui::Ui,
    _h: &HookCx,
    c: &Commit,
    path: &str,
    actions: &mut Vec<Action>,
) {
    use crate::views::menus;
    let sha = c.id.to_string();
    menus::separator(ui);
    menus::action(
        ui,
        "Blame this version",
        actions,
        Act::Blame {
            rev: sha.clone(),
            path: path.to_owned(),
            line: None,
        }
        .into(),
    );
    menus::action(
        ui,
        "Show the file at this commit",
        actions,
        Act::Browse {
            rev: sha.clone(),
            path: Some(path.to_owned()),
        }
        .into(),
    );
    menus::action(
        ui,
        "Compare with the working tree",
        actions,
        Act::Compare {
            a: sha,
            b: None,
            select: Some(path.to_owned()),
        }
        .into(),
    );
}

/// The details panel for several picked commits; true when drawn.
pub fn hook_multi_selection(
    ui: &mut egui::Ui,
    h: &HookCx,
    picked: &[Oid],
    actions: &mut Vec<Action>,
) -> bool {
    picks::panel(ui, h, picked, actions)
}

/// Items for a branch or tag context menu (`name` as shown), at the end of
/// the graph's and the sidebar's ref menus.
pub fn hook_ref_menu(ui: &mut egui::Ui, h: &HookCx, name: &str, actions: &mut Vec<Action>) {
    use crate::views::menus;
    menus::separator(ui);
    if let Some(head) = h.snap.current_branch().map(|b| b.name.clone())
        && head != name
    {
        menus::action(
            ui,
            &format!("Compare {head} with {name}"),
            actions,
            Act::Compare {
                a: name.to_owned(),
                b: Some(head),
                select: None,
            }
            .into(),
        );
    }
    menus::action(
        ui,
        &format!("Compare {name} with…"),
        actions,
        Act::CompareFrom(name.to_owned()).into(),
    );
    menus::action(
        ui,
        "Browse files",
        actions,
        Act::Browse {
            rev: name.to_owned(),
            path: None,
        }
        .into(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acts(cmd: &str, arg: &str) -> Vec<String> {
        script(cmd, arg)
            .unwrap_or_default()
            .iter()
            .map(|a| format!("{a:?}"))
            .collect()
    }

    #[test]
    fn script_commands() {
        assert!(acts("compare", "main devops")[0].contains("Some(\"devops\")"));
        assert!(acts("hist-compare", "main worktree")[0].contains("b: None"));
        assert!(acts("compare", "v1")[0].contains("Some(\"HEAD\")"));
        assert!(script("compare", "").is_none());
        assert!(acts("compare-mode", "three")[0].contains("ThreeDot"));
        assert!(acts("pick-commits", "3")[0].contains("PickFirst(3)"));
        assert!(acts("multi-action", "reword")[0].contains("Reword"));
        assert!(acts("multi-action", "patches")[0].contains("MultiPatches"));
        assert!(script("multi-action", "nonsense").is_none());
        assert!(acts("filter-since", "2026-01-01")[0].contains("Since"));
        assert!(acts("blame-at", "head src/main.rs 12")[0].contains("Some(12)"));
        assert!(acts("blame-previous", "1")[0].contains("BlamePrevious(0)"));
        assert!(acts("browse", "head")[0].contains("\"HEAD\""));
        assert!(acts("hist-mode", "files")[0].contains("Files"));
        assert!(script("verify", "").is_some());
        assert!(script("unknown", "").is_none());
        // Testing aids.
        assert!(acts("browse-line", "120")[0].contains("BrowseLine(120)"));
        assert!(script("browse-line", "x").is_none());
        assert!(acts("commit-msg", "Fix it\\n\\nBody")[0].contains("Fix it\\n\\nBody"));
        assert!(acts("commit-focus", "")[0].contains("FocusCommitMessage"));
        for d in [
            "stash",
            "branch",
            "branch-invalid",
            "tag",
            "rename",
            "delete-branch",
            "push",
            "reset",
            "confirm",
            "confirm-action",
            "clone",
        ] {
            assert!(script("dialog", d).is_some(), "dialog {d}");
        }
        assert!(script("dialog", "nonsense").is_none());
        assert!(acts("toast", "error Push failed")[0].contains("Error"));
        assert!(acts("toast", "Fetched")[0].contains("Info"));
    }

    #[test]
    fn confirm_dialogs_from_scripts_run_nothing() {
        // `dialog confirm` must never be able to run a git operation.
        let Some(Action::Dialog(crate::state::Dialog::Confirm { ops, .. })) =
            test_dialog("confirm")
        else {
            panic!("not a confirmation");
        };
        assert!(ops.is_empty());
    }

    #[test]
    fn prefs_round_trip() {
        let mut s = State::default();
        s.filter.spec.author = "ann".into();
        s.compare.a = "main".into();
        s.compare.mode = RangeMode::ThreeDot;
        let p = s.prefs();
        let json = serde_json::to_string(&p).unwrap();
        let back: Prefs = serde_json::from_str(&json).unwrap();
        let mut t = State::default();
        t.apply_prefs(&back);
        assert_eq!(t.filter.spec.author, "ann");
        assert!(t.filter.open, "an active filter opens the bar");
        assert_eq!(t.compare.mode, RangeMode::ThreeDot);
        let old: Prefs = serde_json::from_str("{}").unwrap();
        assert!(!old.filter.is_active());
    }
}
