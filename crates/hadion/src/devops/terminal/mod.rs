//! The terminal panel (Ctrl+`): real shells in pseudo-terminals, tabs,
//! started in the active repository, with copy / paste and scrollback; and
//! the Terminal section of the Settings page.
//!
//! - Sessions (`hadion_core::devops::terminal::Session`) run on their own
//!   threads; the UI only reads the screen (with `try_term`, so a busy
//!   reader never stalls a frame) and sends bytes through a channel.
//! - Life-cycle work goes through [`Act`] (new, close, restart, cd, clear,
//!   shell discovery). Keystrokes, pastes, mouse reports, scrolling and
//!   selection are the widget's own input and go straight to the session.
//! - Keyboard: while the grid has focus, [`tick`] (which runs before the
//!   app's shortcuts each frame) takes every key, text, copy, cut and paste
//!   event out of egui's queue, so Ctrl+K, Ctrl+L, Ctrl+Z, Ctrl+W, Tab and
//!   the arrows reach the shell instead of the app. Only Ctrl+` is left
//!   alone: it still closes the panel. Click outside the terminal to use
//!   the app's shortcuts again.
//! - Without an open repository the app drops area actions, so the panel
//!   then runs its acts itself (see [`act`]).

mod input;
mod panel;
mod settings;

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use hadion_core::devops::terminal::keys::{self, Modes};
use hadion_core::devops::terminal::screen::{Point, Selection, View};
use hadion_core::devops::terminal::shells::{self, Discovery, Shell};
use hadion_core::devops::terminal::{Colors, Session, SpawnOptions, Status};
use serde::{Deserialize, Serialize};

use crate::devops::ctx::Ctx;
use crate::devops::global::{GlobalCx, GlobalView};
use crate::devops::settings_page::Section;
use crate::state::{Action, Settings};
use crate::theme;

/// Cursor shapes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CursorStyle {
    #[default]
    Block,
    Underline,
    Bar,
}

/// App-wide preferences (`Settings::ext`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Shell id (`pwsh`, `cmd`, `bash`, `wsl`, …); empty = the first found.
    pub default_shell: String,
    pub font_size: f32,
    /// History lines kept per terminal.
    pub scrollback: usize,
    pub cursor: CursorStyle,
    pub blink: bool,
    pub copy_on_select: bool,
    /// New terminals start in the active repository (else the home folder).
    pub start_in_repo: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            default_shell: String::new(),
            font_size: theme::size::BODY,
            scrollback: 10_000,
            cursor: CursorStyle::Block,
            blink: false,
            copy_on_select: false,
            start_in_repo: true,
        }
    }
}

/// Font sizes offered (none below the caption size).
pub const FONT_SIZES: [f32; 11] = [
    11.0, 12.0, 12.5, 13.0, 13.5, 14.0, 15.0, 16.0, 18.0, 20.0, 22.0,
];
/// Scrollback sizes offered.
pub const SCROLLBACKS: [usize; 5] = [1_000, 5_000, 10_000, 50_000, 100_000];

/// Shell discovery.
#[derive(Default)]
pub enum Shells {
    #[default]
    NotLoaded,
    Loading(mpsc::Receiver<Discovery>, Instant),
    Ready(Discovery),
}

impl Shells {
    pub fn ready(&self) -> Option<&Discovery> {
        match self {
            Shells::Ready(d) => Some(d),
            _ => None,
        }
    }
}

/// One terminal tab.
pub struct Tab {
    pub id: u64,
    pub shell: Shell,
    pub cwd: PathBuf,
    pub session: Session,
    /// Times the shell was restarted in this tab.
    pub restarts: u32,
    pub selection: Option<Selection>,
    /// A drag selection is in progress (anchor set, head follows).
    selecting: bool,
    /// The view last drawn and the session generation it was built from.
    view: Option<(u64, View)>,
    /// Title, bell count and status, refreshed in `tick` for every tab
    /// (also the hidden ones) when their session changed.
    meta_gen: u64,
    meta_title: String,
    meta_status: Status,
    bells_seen: usize,
    flash_until: Option<Instant>,
    /// Fractional wheel lines not yet scrolled.
    wheel: f32,
    /// Mouse reporting: the button held and the last cell reported.
    mouse_down: Option<keys::MouseButton>,
    last_mouse_cell: Option<(u16, u16)>,
}

impl Tab {
    /// The latest view (rebuilt only when the session changed; the last one
    /// is kept while the reader holds the lock).
    fn view(&mut self) -> Option<&View> {
        let generation = self.session.generation();
        let stale = self.view.as_ref().is_none_or(|(g, _)| *g != generation);
        if stale && let Some(term) = self.session.try_term() {
            let v = term.screen.view();
            drop(term);
            self.view = Some((generation, v));
        }
        self.view.as_ref().map(|(_, v)| v)
    }

    /// The status as of the last tick (no lock).
    pub fn status(&self) -> &Status {
        &self.meta_status
    }

    /// Refresh title, bells and status when the session changed; returns
    /// true when a bell rang.
    fn refresh_meta(&mut self) -> bool {
        let generation = self.session.generation();
        if generation == self.meta_gen {
            return false;
        }
        let Some(term) = self.session.try_term() else {
            return false;
        };
        let vt = term.screen.vt();
        let (title, bells) = (vt.title().trim().to_owned(), vt.audible_bell_count());
        let status = term.status.clone();
        drop(term);
        self.meta_gen = generation;
        self.meta_title = title;
        self.meta_status = status;
        let rang = bells != self.bells_seen;
        self.bells_seen = bells;
        rang
    }

    /// What the tab says: the program's title, or the shell's name when
    /// the title is only the program path.
    pub fn title(&self) -> String {
        let t = self.meta_title.clone();
        let lower = t.to_ascii_lowercase();
        if t.is_empty() || lower.ends_with(".exe") || lower == "wsl" {
            self.shell.name.clone()
        } else {
            t
        }
    }

    fn modes(&self) -> Modes {
        self.view
            .as_ref()
            .map(|(_, v)| Modes {
                app_cursor: v.app_cursor,
                bracketed_paste: v.bracketed_paste,
            })
            .unwrap_or_default()
    }

    /// Bytes typed by the user: back to the live screen, selection gone.
    fn send_input(&mut self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        if let Some(mut t) = self.session.try_term()
            && t.screen.offset() > 0
        {
            t.screen.set_offset(0);
            drop(t);
            self.session.touch();
        }
        self.selection = None;
        self.session.write(bytes);
    }

    fn selection_text(&self) -> Option<String> {
        let sel = self.selection?;
        let text = self.session.term().screen.selection_text(&sel);
        (!text.is_empty()).then_some(text)
    }

    fn scroll_by(&mut self, lines: isize) {
        self.session.term().screen.scroll_by(lines);
        self.session.touch();
    }

    fn scroll_to_bottom(&mut self) {
        self.session.term().screen.set_offset(0);
        self.session.touch();
    }
}

/// What the area remembers between frames.
#[derive(Default)]
pub struct State {
    pub tabs: Vec<Tab>,
    pub active: usize,
    next_id: u64,
    pub shells: Shells,
    /// `New` requests waiting for shell discovery.
    pending_new: Vec<Option<String>>,
    /// Give the grid keyboard focus on the next frame.
    focus_next: bool,
    /// The grid had keyboard focus in the last frame.
    focused: bool,
    /// The pass the panel was last drawn in (to notice it opening).
    last_pass: Option<u64>,
    /// Scripts: open the "+" menu (`new`) or the grid's context menu.
    open_menu: Option<String>,
    /// Scripts: scroll the Settings section to the detected shells.
    settings_scroll: bool,
    /// Synthetic key events from `term-press`, queued for the next frame.
    injected: Vec<egui::Event>,
    /// A paste asked for from the context menu goes to this tab.
    paste_pending: Option<u64>,
    /// When text was last copied ("Copied" in the tab strip).
    copied_at: Option<Instant>,
    /// Theme the sessions' colours were set for.
    dark: Option<bool>,
    /// Kept from the last tick for acts run without a repository.
    env: Env,
    /// Glyph widths of non-ASCII characters at `glyph_size`.
    glyphs: std::collections::HashMap<char, f32>,
    glyph_size: f32,
}

/// What starting a session needs, refreshed every tick.
#[derive(Clone, Default)]
struct Env {
    egui: Option<egui::Context>,
    prefs: Prefs,
    repo_dir: Option<PathBuf>,
    dark: bool,
}

impl State {
    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    fn tab_index(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// Start shell discovery (once, unless `force`).
    fn discover(&mut self, force: bool) {
        if matches!(self.shells, Shells::Loading(..)) || (!force && self.shells.ready().is_some()) {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let egui = self.env.egui.clone();
        let spawned = std::thread::Builder::new()
            .name("terminal-shells".into())
            .spawn(move || {
                let _ = tx.send(shells::discover());
                if let Some(c) = egui {
                    c.request_repaint();
                }
            });
        if spawned.is_ok() {
            self.shells = Shells::Loading(rx, Instant::now());
        }
    }

    fn colors(dark: bool) -> Colors {
        let p = theme::palette(dark);
        Colors {
            fg: [p.text.r(), p.text.g(), p.text.b()],
            bg: [p.canvas.r(), p.canvas.g(), p.canvas.b()],
        }
    }

    /// Where a new session starts.
    fn start_dir(&self) -> PathBuf {
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from);
        let repo = self
            .env
            .repo_dir
            .clone()
            .filter(|_| self.env.prefs.start_in_repo);
        repo.or(home)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."))
    }

    fn spawn_session(&self, shell: &Shell, cwd: &std::path::Path) -> Session {
        let prefs = &self.env.prefs;
        let opts = SpawnOptions {
            program: shell.program.clone(),
            args: shell.args.clone(),
            cwd: cwd.to_path_buf(),
            // Resized to the panel on the first frame.
            rows: 24,
            cols: 100,
            scrollback: prefs.scrollback,
            // Git Bash's profile stays in the start folder.
            env: vec![("CHERE_INVOKING".into(), "1".into())],
            colors: Self::colors(self.env.dark),
        };
        let egui = self.env.egui.clone();
        Session::spawn(opts, move || {
            if let Some(c) = &egui {
                c.request_repaint();
            }
        })
    }

    fn open_tab(&mut self, shell: Shell) {
        let cwd = self.start_dir();
        let session = self.spawn_session(&shell, &cwd);
        self.next_id += 1;
        self.tabs.push(Tab {
            id: self.next_id,
            shell,
            cwd,
            session,
            restarts: 0,
            selection: None,
            selecting: false,
            view: None,
            meta_gen: u64::MAX,
            meta_title: String::new(),
            meta_status: Status::Starting,
            bells_seen: 0,
            flash_until: None,
            wheel: 0.0,
            mouse_down: None,
            last_mouse_cell: None,
        });
        self.active = self.tabs.len() - 1;
        self.focus_next = true;
    }

    /// Open the shells waiting for discovery.
    fn flush_pending(&mut self) {
        let Some(d) = self.shells.ready().cloned() else {
            return;
        };
        for name in std::mem::take(&mut self.pending_new) {
            let name = name.unwrap_or_else(|| self.env.prefs.default_shell.clone());
            if let Some(shell) = d.pick(&name) {
                self.open_tab(shell.clone());
            }
        }
    }
}

#[derive(Clone, Debug)]
pub enum Act {
    /// A new tab with this shell (id or name); `None` = the default shell.
    New(Option<String>),
    Close(u64),
    CloseActive,
    /// Start the tab's shell again (same shell, same folder).
    Restart(u64),
    RestartActive,
    /// End the active tab's shell (the tab stays, showing the exit code).
    KillActive,
    Activate(usize),
    /// Type the `cd` command for the active repository.
    CdRepo,
    /// Drop the history; shells that redraw on Ctrl+L also clear the screen.
    Clear(u64),
    ClearActive,
    /// Look for shells again.
    Discover,
    /// Scripts: type TEXT and Enter.
    Type(String),
    /// Scripts: a named key (`ctrl-c`, `up`, `tab`).
    Keys(String),
    /// Scripts: scroll the history (lines up; negative = down; 0 = bottom).
    Scroll(isize),
    /// Scripts: select view rows / columns `r0 c0 r1 c1`.
    Select(u16, u16, u16, u16),
    SelectAll,
    /// Copy the selection.
    Copy,
    /// Give the grid keyboard focus.
    Focus,
    /// Scripts: synthetic key presses (`ctrl+l tab g i t enter`), fed to
    /// egui at the start of the next frame like real keys.
    Press(String),
    /// Scripts: open the "+" menu (`new`) or the context menu (`context`).
    Menu(String),
    /// Scripts: scroll Settings > Terminal to the detected shells.
    SettingsScroll,
}

pub enum Msg {}

impl Msg {
    /// One line for the activity log (`Err` when the work failed).
    pub fn summary(&self) -> Result<String, String> {
        match *self {}
    }
}

pub fn apply(_cx: &mut Ctx, st: &mut State, g: &mut GlobalCx, act: Act) {
    refresh_env(st, g);
    if matches!(act, Act::New(_)) {
        g.settings.show_terminal = true;
    }
    handle(st, act);
}

/// Run an act (from `apply`, or from the panel when there is no
/// repository and the app would drop it).
fn handle(st: &mut State, act: Act) {
    match act {
        Act::New(name) => {
            st.pending_new.push(name);
            st.discover(false);
            st.flush_pending();
        }
        Act::Close(id) => {
            if let Some(i) = st.tab_index(id) {
                // Dropping the session ends the shell (off this thread).
                st.tabs.remove(i);
                if st.active > i || st.active >= st.tabs.len() {
                    st.active = st.active.saturating_sub(1);
                }
                st.focus_next = !st.tabs.is_empty();
            }
        }
        Act::CloseActive => {
            if let Some(id) = st.active_tab().map(|t| t.id) {
                handle(st, Act::Close(id));
            }
        }
        Act::Restart(id) => {
            if let Some(i) = st.tab_index(id) {
                let (shell, cwd) = (st.tabs[i].shell.clone(), st.tabs[i].cwd.clone());
                let session = st.spawn_session(&shell, &cwd);
                let tab = &mut st.tabs[i];
                tab.session = session;
                tab.restarts += 1;
                tab.selection = None;
                tab.view = None;
                tab.meta_gen = u64::MAX;
                tab.meta_status = Status::Starting;
                tab.bells_seen = 0;
                st.active = i;
                st.focus_next = true;
            }
        }
        Act::RestartActive => {
            if let Some(id) = st.active_tab().map(|t| t.id) {
                handle(st, Act::Restart(id));
            }
        }
        Act::KillActive => {
            if let Some(t) = st.active_tab() {
                t.session.kill();
            }
        }
        Act::Activate(i) => {
            if i < st.tabs.len() {
                st.active = i;
                st.focus_next = true;
            }
        }
        Act::CdRepo => {
            let Some(dir) = st.env.repo_dir.clone() else {
                return;
            };
            if let Some(t) = st.active_tab_mut() {
                let mut bytes = shells::clear_line(t.shell.kind).to_vec();
                bytes.extend_from_slice(shells::cd_command(t.shell.kind, &dir).as_bytes());
                bytes.push(b'\r');
                t.send_input(bytes);
            }
            st.focus_next = true;
        }
        Act::Clear(id) => {
            if let Some(i) = st.tab_index(id) {
                let tab = &mut st.tabs[i];
                tab.session.term().screen.clear_scrollback();
                tab.session.touch();
                tab.selection = None;
                if tab.shell.kind.clears_with_ctrl_l() && tab.status().is_alive() {
                    tab.session.write(vec![0x0c]);
                }
                st.focus_next = true;
            }
        }
        Act::ClearActive => {
            if let Some(id) = st.active_tab().map(|t| t.id) {
                handle(st, Act::Clear(id));
            }
        }
        Act::Discover => st.discover(true),
        Act::Type(text) => {
            if let Some(t) = st.active_tab_mut() {
                let mut bytes = text.into_bytes();
                bytes.push(b'\r');
                t.send_input(bytes);
            }
        }
        Act::Keys(name) => {
            if let Some(t) = st.active_tab_mut() {
                t.view();
                if let Some(bytes) = keys::encode_named(&name, t.modes()) {
                    t.send_input(bytes);
                }
            }
        }
        Act::Scroll(n) => {
            if let Some(t) = st.active_tab_mut() {
                if n == 0 {
                    t.scroll_to_bottom();
                } else {
                    t.scroll_by(n);
                }
            }
        }
        Act::Select(r0, c0, r1, c1) => {
            if let Some(t) = st.active_tab_mut() {
                let top = t.session.term().screen.view().top_row;
                let p = |r: u16, c: u16| Point {
                    row: top + r as usize,
                    col: c,
                };
                t.selection = Some(Selection::new(p(r0, c0), p(r1, c1)));
            }
        }
        Act::SelectAll => {
            if let Some(t) = st.active_tab_mut() {
                t.selection = Some(t.session.term().screen.select_all());
            }
        }
        Act::Copy => copy_selection(st),
        Act::Focus => st.focus_next = true,
        Act::Press(spec) => st.injected.extend(input::parse_press(&spec)),
        Act::Menu(which) => st.open_menu = Some(which),
        Act::SettingsScroll => st.settings_scroll = true,
    }
}

/// Copy the active tab's selection to the clipboard.
fn copy_selection(st: &mut State) {
    let Some(text) = st.active_tab().and_then(Tab::selection_text) else {
        return;
    };
    if let Some(c) = &st.env.egui {
        c.copy_text(text);
        st.copied_at = Some(Instant::now());
    }
}

/// Run `a` now when the app would drop it (no repository open), otherwise
/// queue it as an action.
fn act(st: &mut State, v: &GlobalView, actions: &mut Vec<Action>, a: Act) {
    if v.repos.is_empty() {
        handle(st, a);
    } else {
        actions.push(a.into());
    }
}

pub fn on_msg(_cx: &mut Ctx, _st: &mut State, _g: &mut GlobalCx, msg: Msg) {
    match msg {}
}

fn refresh_env(st: &mut State, g: &GlobalCx) {
    if st.env.egui.is_none() {
        st.env.egui = Some(g.egui.clone());
    }
    if st.env.prefs != g.settings.ext.terminal {
        st.env.prefs = g.settings.ext.terminal.clone();
    }
    let dir = g.active_repo().map(|r| &r.workdir);
    if st.env.repo_dir.as_ref() != dir {
        st.env.repo_dir = dir.cloned();
    }
    st.env.dark = g.settings.dark;
}

/// Every frame; keep it cheap.
pub fn tick(_cx: &mut Ctx, st: &mut State, g: &mut GlobalCx) {
    refresh_env(st, g);
    if let Shells::Loading(rx, _) = &st.shells
        && let Ok(d) = rx.try_recv()
    {
        st.shells = Shells::Ready(d);
        st.flush_pending();
    }
    if st.dark != Some(g.settings.dark) {
        st.dark = Some(g.settings.dark);
        let colors = State::colors(g.settings.dark);
        for t in &st.tabs {
            t.session.set_colors(colors);
        }
    }
    // A bell flashes its tab.
    let now = Instant::now();
    for t in &mut st.tabs {
        if t.refresh_meta() {
            t.flash_until = Some(now + Duration::from_millis(900));
            g.egui.request_repaint();
        }
    }
    if g.settings.show_terminal {
        input::take_keys(st, g.egui);
    }
}

/// `HADION_SCRIPT` commands this area understands: `term-new SHELL`,
/// `term-type TEXT` (then Enter), `term-keys NAME`, `term-tab N`,
/// `term-close`, `term-scroll N` (lines up; negative down; 0 bottom),
/// `term-select R0 C0 R1 C1` (view rows / columns), `term-select-all`,
/// `term-copy`, `term-focus`, `term-restart`, `term-kill`, `term-cd`,
/// `term-clear`, `term-shells`, `term-menu new|context` (the context menu
/// opens only while the pointer is over the window),
/// `term-settings-scroll` (Settings → Terminal down to the shells),
/// `term-press KEYS` (synthetic key presses, e.g. `ctrl+l tab e c h o
/// space h i enter`, which meet the app's shortcuts like real keys).
pub fn script(cmd: &str, arg: &str) -> Option<Vec<Action>> {
    let num = |s: &str| s.trim().parse::<isize>().ok();
    let a = match cmd {
        "term-new" => Act::New((!arg.trim().is_empty()).then(|| arg.trim().to_owned())),
        "term-type" => Act::Type(arg.to_owned()),
        "term-keys" => Act::Keys(arg.trim().to_owned()),
        "term-tab" => Act::Activate(num(arg)?.max(0) as usize),
        "term-close" => Act::CloseActive,
        "term-scroll" => Act::Scroll(num(arg).unwrap_or(0)),
        "term-select" => {
            let n: Vec<u16> = arg
                .split_whitespace()
                .filter_map(|s| s.parse().ok())
                .collect();
            let [r0, c0, r1, c1] = n[..] else {
                return None;
            };
            Act::Select(r0, c0, r1, c1)
        }
        "term-select-all" => Act::SelectAll,
        "term-copy" => Act::Copy,
        "term-focus" => Act::Focus,
        "term-restart" => Act::RestartActive,
        "term-kill" => Act::KillActive,
        "term-cd" => Act::CdRepo,
        "term-clear" => Act::ClearActive,
        "term-shells" => Act::Discover,
        "term-press" => Act::Press(arg.to_owned()),
        "term-menu" => Act::Menu(arg.trim().to_owned()),
        "term-settings-scroll" => Act::SettingsScroll,
        _ => return None,
    };
    Some(vec![a.into()])
}

/// A Settings section became visible (check `section`; most are not yours).
pub fn on_section_shown(_cx: &mut Ctx, st: &mut State, g: &mut GlobalCx, section: Section) {
    if section == Section::Terminal {
        refresh_env(st, g);
        st.discover(false);
    }
}

/// The panel (a bottom panel while `settings.show_terminal`).
pub fn show_panel(ui: &mut egui::Ui, st: &mut State, v: &GlobalView, actions: &mut Vec<Action>) {
    panel::show(ui, st, v, actions);
}

pub fn settings_section(
    ui: &mut egui::Ui,
    st: &mut State,
    settings: &mut Settings,
    actions: &mut Vec<Action>,
) {
    settings::show(ui, st, settings, actions);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_commands() {
        let one = |c: &str, a: &str| script(c, a).map(|v| v.len());
        assert_eq!(one("term-new", "cmd"), Some(1));
        assert_eq!(one("term-new", ""), Some(1));
        assert_eq!(one("term-type", "git status"), Some(1));
        assert_eq!(one("term-keys", "ctrl-c"), Some(1));
        assert_eq!(one("term-tab", "1"), Some(1));
        assert_eq!(one("term-tab", "x"), None);
        assert_eq!(one("term-select", "0 0 2 10"), Some(1));
        assert_eq!(one("term-select", "0 0 2"), None);
        assert_eq!(one("term-scroll", "-5"), Some(1));
        assert_eq!(one("term-close", ""), Some(1));
        assert_eq!(one("unrelated", ""), None);
        match &script("term-new", " bash ").unwrap()[0] {
            Action::DevOps(crate::devops::DevOpsAction::Terminal(Act::New(Some(s)))) => {
                assert_eq!(s, "bash")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn prefs_defaults_survive_partial_json() {
        let p: Prefs = serde_json::from_str(r#"{"font_size": 15.0}"#).unwrap();
        assert_eq!(p.font_size, 15.0);
        assert_eq!(p.scrollback, 10_000);
        assert!(p.start_in_repo);
        assert_eq!(p.cursor, CursorStyle::Block);
    }
}
