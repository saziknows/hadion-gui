//! The rows the palette shows for its input: search results, the Recent
//! and Suggested groups of an empty input, the actions of one item, the
//! prefix help, and the prefixes again when nothing matches. Computed when
//! the input or the data changes, not every frame.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};

use gitvis_core::devops::palette::frecency::{self, Use};
use gitvis_core::devops::palette::rank;
use gitvis_core::devops::palette::{Haystack, Item, Kind, Matcher, Query, Scope, Target, scope};

use super::sources::Src;
use super::verbs::{self, Verb};
use super::{Cx, State};

/// Most rows a search keeps (the list scrolls; the footer says how many
/// matched in all).
pub const LIMIT: usize = 500;
/// Rows of the Recent and Suggested groups.
pub const RECENT: usize = 6;
pub const SUGGESTED: usize = 6;

pub enum Row {
    /// A caption between groups (not selectable).
    Header(&'static str),
    Entry(Entry),
}

#[derive(Clone)]
pub struct Entry {
    pub item: Item,
    /// Run this instead of the item's default action (a recent use, a row
    /// of an item's actions).
    pub verb: Option<Verb>,
    /// Shown instead of the item's title (an action's sentence).
    pub title: Option<String>,
    /// From the Recent group.
    pub recent: bool,
    /// Characters of the shown title to emphasise (computed when drawn).
    pub hl: Option<Vec<u32>>,
}

impl Entry {
    fn of(item: Item) -> Self {
        Self {
            item,
            verb: None,
            title: None,
            recent: false,
            hl: None,
        }
    }

    pub fn shown_title(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.item.title)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    /// Matches of a query (or everything of a scope).
    #[default]
    Search,
    /// Empty input: Recent and Suggested.
    Start,
    /// The actions of one item.
    Actions,
    /// Nothing matched: the prefixes are offered instead.
    NoResults,
    /// `?`: the prefixes.
    Help,
}

#[derive(Default)]
pub struct Results {
    pub rows: Vec<Row>,
    /// What the rows were computed for.
    key: Option<u64>,
    pub mode: Mode,
    pub scope: Scope,
    /// The query the titles are highlighted with.
    pub query: Query,
    pub matched: usize,
    pub searched: usize,
    pub micros: u64,
    pub narrowed: bool,
}

impl Results {
    pub fn entry(&self, i: usize) -> Option<&Entry> {
        match self.rows.get(i) {
            Some(Row::Entry(e)) => Some(e),
            _ => None,
        }
    }

    /// The first selectable row at or after `i` (or before it, at the end).
    pub fn selectable(&self, i: usize) -> usize {
        let n = self.rows.len();
        if n == 0 {
            return 0;
        }
        let i = i.min(n - 1);
        (i..n)
            .find(|&j| matches!(self.rows[j], Row::Entry(_)))
            .or_else(|| (0..i).rev().find(|&j| matches!(self.rows[j], Row::Entry(_))))
            .unwrap_or(0)
    }

    /// Move `from` by `delta` selectable rows (clamped).
    pub fn step(&self, from: usize, delta: i64) -> usize {
        let entries: Vec<usize> = (0..self.rows.len())
            .filter(|&j| matches!(self.rows[j], Row::Entry(_)))
            .collect();
        if entries.is_empty() {
            return 0;
        }
        let pos = entries.iter().position(|&j| j >= from).unwrap_or(entries.len() - 1) as i64;
        let to = (pos + delta).clamp(0, entries.len() as i64 - 1) as usize;
        entries[to]
    }

    pub fn entries(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| matches!(r, Row::Entry(_)))
            .count()
    }
}

/// The sources the current input needs.
pub fn needed(st: &State) -> &'static [Src] {
    if st.drill.is_some() {
        return &[];
    }
    match scope::parse(&st.query) {
        (Scope::All, "") => &[Src::Commands, Src::Tasks, Src::Refs, Src::Runs],
        (s, _) => Src::for_scope(s),
    }
}

/// Recompute the rows when the input or the data changed.
pub fn refresh(st: &mut State, cx: &Cx) {
    let srcs = needed(st);
    st.cache.ensure(cx, &st.files, srcs);
    let (scope, text) = scope::parse(&st.query);
    let uses = &cx.settings.ext.palette.recent;
    let repo_key = cx.repo_key();

    let mut h = std::collections::hash_map::DefaultHasher::new();
    st.query.hash(&mut h);
    st.drill
        .as_ref()
        .map(|d| (&d.item.key, d.recent))
        .hash(&mut h);
    for s in Src::ALL {
        st.cache.stamp(s).hash(&mut h);
    }
    (&repo_key, uses.len()).hash(&mut h);
    for u in uses {
        (&u.key, u.count, u.last).hash(&mut h);
    }
    cx.head_branch().hash(&mut h);
    let key = h.finish();
    if st.results.key == Some(key) {
        return;
    }

    let mut out = Results {
        key: Some(key),
        scope,
        query: Query::new(text),
        ..Default::default()
    };
    if let Some(d) = &st.drill {
        out.mode = Mode::Actions;
        out.rows = action_rows(&d.item, d.recent, text, cx, &st.cache.catalog);
        out.matched = out.rows.len();
    } else if scope == Scope::All && text.is_empty() {
        out.mode = Mode::Start;
        out.rows = start_rows(st, cx, uses, &repo_key);
    } else {
        let now = crate::format::now_secs();
        let values = frecency::values(uses, &repo_key, now);
        let sources: Vec<(&[Item], u64)> = Src::for_scope(scope)
            .iter()
            .map(|s| (st.cache.items(*s), st.cache.stamp(*s)))
            .collect();
        let r = st
            .ranker
            .rank(&sources, scope, &out.query, &values, LIMIT);
        out.matched = r.matched;
        out.searched = r.searched;
        out.micros = r.micros;
        out.narrowed = r.narrowed;
        out.rows = r
            .hits
            .iter()
            .map(|h| Row::Entry(Entry::of(sources[h.source as usize].0[h.index as usize].clone())))
            .collect();
        out.mode = if scope == Scope::Help {
            Mode::Help
        } else {
            Mode::Search
        };
        if out.rows.is_empty() && scope != Scope::Help {
            out.mode = Mode::NoResults;
            out.rows = prefix_rows(scope, text);
        }
    }
    // A new query starts at the top; the same query (the data changed)
    // keeps the highlighted row where it can.
    let same = st.results.key.is_some()
        && st.results.query == out.query
        && st.results.mode == out.mode
        && st.results.scope == out.scope;
    if same {
        st.selected = out.selectable(st.selected);
    } else {
        st.selected = out.selectable(0);
        st.scroll_offset = 0.0;
    }
    st.results = out;
}

/// The Recent and Suggested groups.
fn start_rows(st: &State, cx: &Cx, uses: &[Use], repo_key: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut shown = HashSet::new();
    let recent: Vec<Entry> = frecency::recent(uses, repo_key, RECENT * 3)
        .into_iter()
        .filter_map(|u| resolve(u, st, cx))
        .filter(|e| shown.insert(e.item.key.clone()))
        .take(RECENT)
        .collect();
    if !recent.is_empty() {
        rows.push(Row::Header("Recent"));
        rows.extend(recent.into_iter().map(Row::Entry));
    }
    let suggested: Vec<Entry> = suggestions(st, cx)
        .into_iter()
        .filter(|e| shown.insert(e.item.key.clone()))
        .take(SUGGESTED)
        .collect();
    if !suggested.is_empty() {
        rows.push(Row::Header("Suggested"));
        rows.extend(suggested.into_iter().map(Row::Entry));
    }
    rows
}

/// A remembered use as a row, when what it points at still exists.
fn resolve(u: &Use, st: &State, cx: &Cx) -> Option<Entry> {
    let target = Target::from_key(&u.key)?;
    let kind = target.kind();
    let src = match kind {
        Kind::Command => Some(Src::Commands),
        Kind::Branch | Kind::Remote | Kind::Tag => Some(Src::Refs),
        Kind::Task => Some(Src::Tasks),
        _ => None,
    };
    let item = match src {
        // These must exist now (a command available here, a branch that
        // was not deleted, a task that was found).
        Some(s) => st.cache.find(s, u.entity())?.clone(),
        None => {
            if let Target::Repo(p) = &target
                && !p.exists()
            {
                return None;
            }
            Item::new(target, u.title.clone(), u.subtitle.clone())
        }
    };
    let catalog = &st.cache.catalog;
    let verb = u.verb().and_then(Verb::from_id);
    let available = verbs::verbs(&item, cx, catalog);
    if available.is_empty() {
        return None;
    }
    let verb = verb.filter(|v| available.contains(v));
    let title = verb
        .filter(|v| Some(*v) != available.first().copied())
        .map(|v| verbs::label(v, &item, cx, catalog));
    Some(Entry {
        item,
        verb,
        title,
        recent: true,
        hl: None,
    })
}

/// What is likely next: finish a merge, pull what is behind, push what is
/// ahead, look at the changes, the newest failed run of this branch, …
fn suggestions(st: &State, cx: &Cx) -> Vec<Entry> {
    let mut ids: Vec<&str> = Vec::new();
    if let Some(snap) = cx.snap() {
        ids.push("git:continue");
        if let Some(b) = snap.current_branch() {
            if b.behind > 0 {
                ids.push("git:pull");
            }
            if b.ahead > 0 || b.upstream.is_none() {
                ids.push("git:push");
            }
        }
        ids.push("git:wip");
        ids.push("git:pop");
    }
    let mut out: Vec<Entry> = ids
        .into_iter()
        .filter_map(|id| st.cache.find(Src::Commands, &format!("cmd:{id}")))
        .map(|i| Entry::of(i.clone()))
        .collect();
    // The newest failed run of the current branch.
    if let (Some(branch), Some(gh)) = (cx.head_branch(), cx.repo().and_then(|r| r.github.as_ref()))
        && let Some(run) = gh
            .runs
            .iter()
            .find(|r| r.branch == branch && r.outcome().is_bad())
        && let Some(item) = st.cache.find(Src::Runs, &format!("run:{}", run.id))
    {
        out.push(Entry::of(item.clone()));
    }
    for id in ["git:fetch", "app:open", "app:theme", "palette:help"] {
        if let Some(i) = st.cache.find(Src::Commands, &format!("cmd:{id}")) {
            out.push(Entry::of(i.clone()));
        }
    }
    out
}

/// The actions of `item`, filtered by `text`.
fn action_rows(
    item: &Item,
    recent: bool,
    text: &str,
    cx: &Cx,
    catalog: &super::commands::Catalog,
) -> Vec<Row> {
    let mut vs = verbs::verbs(item, cx, catalog);
    if recent {
        vs.push(Verb::Forget);
    }
    let q = Query::new(text);
    let mut m = Matcher::new();
    let mut scored: Vec<(i32, usize, Entry)> = vs
        .into_iter()
        .enumerate()
        .filter_map(|(i, v)| {
            let label = verbs::label(v, item, cx, catalog);
            let s = if q.is_empty() {
                0
            } else {
                m.score(&q, &Haystack::new(&label))?
            };
            Some((
                s,
                i,
                Entry {
                    item: item.clone(),
                    verb: Some(v),
                    title: Some(label),
                    recent,
                    hl: None,
                },
            ))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, e)| Row::Entry(e)).collect()
}

/// When nothing matched: search one kind instead (keeping the text).
fn prefix_rows(current: Scope, text: &str) -> Vec<Row> {
    let mut rows = vec![Row::Header("Search one kind instead")];
    for p in scope::PREFIXES
        .iter()
        .filter(|p| p.scope != Scope::Help && p.scope != current)
    {
        let item = gitvis_core::devops::palette::sources::prefix_items()
            .into_iter()
            .find(|i| i.target == Target::Prefix(p.scope))
            .expect("every prefix has an item");
        let title = if text.is_empty() {
            None
        } else {
            Some(format!("Search {} for \"{text}\"", p.label.to_lowercase()))
        };
        rows.push(Row::Entry(Entry {
            item,
            verb: Some(Verb::UsePrefix),
            title,
            recent: false,
            hl: None,
        }));
    }
    rows
}

/// Emphasised characters of a row's title.
pub fn highlight(m: &mut Matcher, mode: Mode, query: &Query, e: &Entry) -> Vec<u32> {
    if query.is_empty() {
        return Vec::new();
    }
    match (&e.title, mode) {
        (Some(t), Mode::Actions) => {
            let h = Haystack::new(t);
            let mut out = Vec::new();
            for tok in query.tokens() {
                let _ = m.token(tok, &h, Some(&mut out));
            }
            out.sort_unstable();
            out.dedup();
            out
        }
        (Some(_), _) => Vec::new(),
        (None, _) => rank::title_indices(m, query, &e.item),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{RepoState, Settings};

    fn state_with(query: &str) -> State {
        State {
            open: true,
            query: query.to_owned(),
            ..Default::default()
        }
    }

    #[test]
    fn empty_input_suggests_and_lists_recent() {
        let mut settings = Settings::default();
        frecency::record(
            &mut settings.ext.palette.recent,
            "cmd:app:theme",
            "",
            "Switch theme",
            "",
            crate::format::now_secs(),
        );
        frecency::record(
            &mut settings.ext.palette.recent,
            "cmd:no-such-command",
            "",
            "Gone",
            "",
            crate::format::now_secs(),
        );
        let repos = [RepoState::new(1, "C:/x".into())];
        let cx = Cx {
            settings: &settings,
            repos: &repos,
            active: 0,
        };
        let mut st = state_with("");
        refresh(&mut st, &cx);
        assert_eq!(st.results.mode, Mode::Start);
        assert!(matches!(st.results.rows[0], Row::Header("Recent")));
        // The command that no longer exists is left out.
        let recent: Vec<&str> = st
            .results
            .rows
            .iter()
            .skip(1)
            .take_while(|r| matches!(r, Row::Entry(_)))
            .filter_map(|r| match r {
                Row::Entry(e) => Some(e.item.key.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(recent, ["cmd:app:theme"]);
        assert!(
            st.results
                .rows
                .iter()
                .any(|r| matches!(r, Row::Header("Suggested")))
        );
        // The theme is not suggested again under Suggested.
        let themes = st
            .results
            .rows
            .iter()
            .filter(|r| matches!(r, Row::Entry(e) if e.item.key == "cmd:app:theme"))
            .count();
        assert_eq!(themes, 1);
        // The highlight starts on the first entry, not the header.
        assert_eq!(st.selected, 1);
    }

    #[test]
    fn search_and_no_results() {
        let settings = Settings::default();
        let repos = [RepoState::new(1, "C:/x".into())];
        let cx = Cx {
            settings: &settings,
            repos: &repos,
            active: 0,
        };
        let mut st = state_with(">theme");
        refresh(&mut st, &cx);
        assert_eq!(st.results.mode, Mode::Search);
        assert_eq!(
            st.results.entry(0).map(|e| e.item.key.as_str()),
            Some("cmd:app:theme")
        );
        let mut st = state_with(">qqqzzz");
        refresh(&mut st, &cx);
        assert_eq!(st.results.mode, Mode::NoResults);
        assert!(matches!(st.results.rows[0], Row::Header(_)));
        let e = st.results.entry(1).unwrap();
        assert_eq!(e.verb, Some(Verb::UsePrefix));
        assert!(e.shown_title().contains("qqqzzz"));
        // The prefix that was searched is not offered again.
        assert!(!st.results.rows.iter().any(
            |r| matches!(r, Row::Entry(e) if e.item.target == Target::Prefix(Scope::Commands))
        ));
        let mut st = state_with("?");
        refresh(&mut st, &cx);
        assert_eq!(st.results.mode, Mode::Help);
        assert_eq!(st.results.entries(), scope::PREFIXES.len());
    }

    #[test]
    fn actions_of_an_item() {
        let settings = Settings::default();
        let repos = [RepoState::new(1, "C:/x".into())];
        let cx = Cx {
            settings: &settings,
            repos: &repos,
            active: 0,
        };
        let item = gitvis_core::devops::palette::sources::file_items(&["src/a.rs".into()])
            .remove(0);
        let mut st = state_with("");
        st.drill = Some(super::super::Drill {
            item,
            recent: true,
            query: "/a".into(),
            selected: 0,
        });
        refresh(&mut st, &cx);
        assert_eq!(st.results.mode, Mode::Actions);
        let verbs: Vec<Verb> = (0..st.results.rows.len())
            .filter_map(|i| st.results.entry(i).and_then(|e| e.verb))
            .collect();
        assert_eq!(
            verbs,
            [
                Verb::History,
                Verb::OpenFile,
                Verb::CopyPath,
                Verb::Forget
            ]
        );
        st.query = "copy".into();
        refresh(&mut st, &cx);
        assert_eq!(st.results.entry(0).and_then(|e| e.verb), Some(Verb::CopyPath));
    }

    #[test]
    fn selection_skips_headers() {
        let r = Results {
            rows: vec![
                Row::Header("A"),
                Row::Entry(Entry::of(Item::new(Target::Pr(1), "x", ""))),
                Row::Header("B"),
                Row::Entry(Entry::of(Item::new(Target::Pr(2), "y", ""))),
            ],
            ..Default::default()
        };
        assert_eq!(r.selectable(0), 1);
        assert_eq!(r.selectable(2), 3);
        assert_eq!(r.step(1, 1), 3);
        assert_eq!(r.step(3, 1), 3);
        assert_eq!(r.step(3, -5), 1);
        assert_eq!(r.entries(), 2);
    }
}
