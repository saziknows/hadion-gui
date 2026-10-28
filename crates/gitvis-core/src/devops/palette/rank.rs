//! Ranking: match every item of the searched scope, add the kind weight
//! and frecency, keep the best `limit`.
//!
//! A query that only grew at its end (`ma` → `mai`) can only match a subset
//! of what the previous one matched, so [`Ranker`] keeps the previous
//! matches and searches only those while the sources are unchanged.

use std::collections::HashMap;
use std::time::Instant;

use super::frecency;
use super::fuzzy::{Matcher, Query};
use super::item::Item;
use super::scope::Scope;

/// A token that starts an item's exact key (a SHA, a number) scores this
/// much plus [`EXACT_PER_CHAR`] per character.
pub const EXACT_BASE: i32 = 40;
pub const EXACT_PER_CHAR: i32 = 24;

/// One ranked item: `sources[source][index]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub source: u16,
    pub index: u32,
    pub score: i32,
}

/// A search result.
#[derive(Clone, Debug, Default)]
pub struct Ranked {
    /// The best hits, best first, at most `limit`.
    pub hits: Vec<Hit>,
    /// How many items matched in all.
    pub matched: usize,
    /// How many items were tried.
    pub searched: usize,
    /// Microseconds the search took.
    pub micros: u64,
    /// Only the previous query's matches were searched.
    pub narrowed: bool,
}

/// Added when two or more tokens match the title in the order they were
/// typed (`fix crash` before "Crash fix").
pub const ORDER_BONUS: i32 = 8;

/// The text score of `item` (weight and frecency not included) when every
/// token appears in the title, the keywords or the exact key, and whether
/// every token also matches well somewhere (see `Matcher::token_quality`).
/// Only good items are listed; the others still count as matches for
/// narrowing, which needs the plain subsequence test to stay monotonic.
pub fn score_item(m: &mut Matcher, q: &Query, item: &Item) -> Option<(i32, bool)> {
    let mut total = 0;
    let mut good = true;
    let mut starts: Vec<u32> = Vec::new();
    for t in q.tokens() {
        let mut best: Option<i32> = None;
        let mut ok = false;
        if let Some(e) = &item.exact
            && t.len() >= item.exact_min
            && e.starts_with(t.text())
        {
            best = Some(EXACT_BASE + EXACT_PER_CHAR * t.len() as i32);
            ok = true;
        }
        if let Some(tm) = m.token_quality(t, &item.hay) {
            best = Some(best.map_or(tm.score, |b| b.max(tm.score)));
            ok |= tm.good;
            starts.push(tm.start);
        }
        if let Some(x) = &item.extra
            && let Some(tm) = m.token_quality(t, x)
        {
            ok |= tm.good;
            let s = tm.score + item.extra_adjust;
            best = Some(best.map_or(s, |b| b.max(s)));
        }
        total += best?;
        good &= ok;
    }
    if starts.len() >= 2 && starts.windows(2).all(|w| w[0] < w[1]) {
        total += ORDER_BONUS;
    }
    Some((total, good))
}

/// Character indices of the title to emphasise: the alignment of every
/// token that matches the title.
pub fn title_indices(m: &mut Matcher, q: &Query, item: &Item) -> Vec<u32> {
    let mut out = Vec::new();
    for t in q.tokens() {
        let _ = m.token(t, &item.hay, Some(&mut out));
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Frecency points of an item.
pub fn frecency_bonus(values: &HashMap<String, f64>, item: &Item) -> i32 {
    values
        .get(item.key.as_str())
        .map(|v| frecency::bonus(*v))
        .unwrap_or(0)
}

struct Last {
    scope: Scope,
    query: Query,
    stamps: Vec<u64>,
    matched: Vec<(u16, u32)>,
}

/// Searches sources, keeping the previous matches for narrowing queries.
#[derive(Default)]
pub struct Ranker {
    matcher: Matcher,
    last: Option<Last>,
}

impl Ranker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget the previous matches (the next search reads everything).
    pub fn reset(&mut self) {
        self.last = None;
    }

    /// The matcher, for highlighting the rows that are drawn.
    pub fn matcher(&mut self) -> &mut Matcher {
        &mut self.matcher
    }

    /// Rank the items of `scope` in `sources` (`(items, stamp)`; a changed
    /// stamp means the items changed) for `query`.
    pub fn rank(
        &mut self,
        sources: &[(&[Item], u64)],
        scope: Scope,
        query: &Query,
        values: &HashMap<String, f64>,
        limit: usize,
    ) -> Ranked {
        let started = Instant::now();
        let stamps: Vec<u64> = sources.iter().map(|(_, s)| *s).collect();
        let mut hits = Vec::new();
        let mut matched = Vec::new();
        let mut searched = 0usize;
        let mut narrowed = false;
        if query.is_empty() {
            // Everything in scope, by weight and frecency, then source order.
            for (s, (items, _)) in sources.iter().enumerate() {
                for (i, item) in items.iter().enumerate() {
                    if item.kind().in_scope(scope) {
                        searched += 1;
                        hits.push(Hit {
                            source: s as u16,
                            index: i as u32,
                            score: item.weight + frecency_bonus(values, item),
                        });
                    }
                }
            }
            self.last = None;
        } else {
            let reuse = self
                .last
                .as_ref()
                .is_some_and(|l| l.scope == scope && l.stamps == stamps && query.narrows(&l.query));
            let mut try_item = |s: u16, i: u32, item: &Item, m: &mut Matcher| {
                if let Some((text, good)) = score_item(m, query, item) {
                    matched.push((s, i));
                    if good {
                        hits.push(Hit {
                            source: s,
                            index: i,
                            score: text + item.weight + frecency_bonus(values, item),
                        });
                    }
                }
            };
            if reuse {
                narrowed = true;
                let prev = std::mem::take(&mut self.last.as_mut().expect("checked").matched);
                for &(s, i) in &prev {
                    if let Some(item) = sources
                        .get(s as usize)
                        .and_then(|(items, _)| items.get(i as usize))
                    {
                        searched += 1;
                        try_item(s, i, item, &mut self.matcher);
                    }
                }
            } else {
                for (s, (items, _)) in sources.iter().enumerate() {
                    for (i, item) in items.iter().enumerate() {
                        if item.kind().in_scope(scope) {
                            searched += 1;
                            try_item(s as u16, i as u32, item, &mut self.matcher);
                        }
                    }
                }
            }
            self.last = Some(Last {
                scope,
                query: query.clone(),
                stamps,
                matched,
            });
        }
        let total = hits.len();
        let item = |h: &Hit| &sources[h.source as usize].0[h.index as usize];
        let by_text = !query.is_empty();
        let cmp = |a: &Hit, b: &Hit| {
            b.score
                .cmp(&a.score)
                .then_with(|| {
                    if by_text {
                        item(a).sort_len.cmp(&item(b).sort_len)
                    } else {
                        std::cmp::Ordering::Equal
                    }
                })
                .then_with(|| a.source.cmp(&b.source))
                .then_with(|| item(a).order.cmp(&item(b).order))
        };
        if limit > 0 && hits.len() > limit {
            hits.select_nth_unstable_by(limit - 1, cmp);
            hits.truncate(limit);
        }
        hits.sort_unstable_by(cmp);
        Ranked {
            hits,
            matched: total,
            searched,
            micros: started.elapsed().as_micros() as u64,
            narrowed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RefKind;
    use crate::devops::palette::item::Target;
    use crate::devops::palette::sources::{self, CommandEntry, CommitEntry, RefEntry};

    fn cmd(id: &str, title: &str) -> Item {
        sources::command_items(&[CommandEntry {
            id: id.into(),
            title: title.into(),
            category: "Git".into(),
            ..Default::default()
        }])
        .remove(0)
    }

    fn titles(r: &Ranked, sources: &[(&[Item], u64)]) -> Vec<String> {
        r.hits
            .iter()
            .map(|h| sources[h.source as usize].0[h.index as usize].title.clone())
            .collect()
    }

    #[test]
    fn orders_by_score_then_length() {
        let items = vec![
            cmd("a", "Stash and push"),
            cmd("b", "Push to origin"),
            cmd("c", "Push"),
            cmd("d", "Unpushed commits"),
            cmd("e", "Pull"),
        ];
        let src = [(items.as_slice(), 1)];
        let mut r = Ranker::new();
        let res = r.rank(&src, Scope::All, &Query::new("push"), &HashMap::new(), 10);
        assert_eq!(
            titles(&res, &src),
            [
                "Push",
                "Push to origin",
                "Stash and push",
                "Unpushed commits"
            ]
        );
        assert_eq!(res.matched, 4);
        assert_eq!(res.searched, 5);
    }

    #[test]
    fn frecency_reorders_near_ties_only() {
        let items = vec![cmd("a", "Push"), cmd("b", "Push tags")];
        let src = [(items.as_slice(), 1)];
        let mut values = HashMap::new();
        values.insert("cmd:b".to_owned(), 5.0);
        let mut r = Ranker::new();
        let res = r.rank(&src, Scope::All, &Query::new("push"), &values, 10);
        assert_eq!(titles(&res, &src), ["Push tags", "Push"]);
        // A much better match is not overtaken.
        let items = vec![cmd("a", "Push"), cmd("b", "Pick a useful shortcut here")];
        let src = [(items.as_slice(), 1)];
        let res = r.rank(&src, Scope::All, &Query::new("push"), &values, 10);
        assert_eq!(titles(&res, &src)[0], "Push");
    }

    #[test]
    fn scope_filters_kinds_and_weights_break_ties() {
        let refs = sources::ref_items(&[
            RefEntry {
                name: "origin/main".into(),
                full: "refs/remotes/origin/main".into(),
                kind: Some(RefKind::Remote),
                ..Default::default()
            },
            RefEntry {
                name: "main".into(),
                full: "refs/heads/main".into(),
                kind: Some(RefKind::Local),
                ..Default::default()
            },
        ]);
        let cmds = vec![cmd("x", "Main menu")];
        let src = [(refs.as_slice(), 1), (cmds.as_slice(), 1)];
        let mut r = Ranker::new();
        let res = r.rank(&src, Scope::Refs, &Query::new("main"), &HashMap::new(), 10);
        assert_eq!(titles(&res, &src), ["main", "origin/main"]);
        let res = r.rank(
            &src,
            Scope::Commands,
            &Query::new("main"),
            &HashMap::new(),
            10,
        );
        assert_eq!(titles(&res, &src), ["Main menu"]);
        let res = r.rank(&src, Scope::All, &Query::new("main"), &HashMap::new(), 10);
        assert_eq!(res.matched, 3);
    }

    #[test]
    fn exact_keys_and_keywords() {
        let commits = sources::commit_items(&[
            CommitEntry {
                sha: "deadbeef00".into(),
                short: "deadbee".into(),
                summary: "Unrelated change".into(),
                author: "Grace".into(),
                ..Default::default()
            },
            CommitEntry {
                sha: "0123456789".into(),
                short: "0123456".into(),
                summary: "Add deadline handling".into(),
                author: "Alan".into(),
                ..Default::default()
            },
        ]);
        let src = [(commits.as_slice(), 1)];
        let mut r = Ranker::new();
        let res = r.rank(
            &src,
            Scope::Commits,
            &Query::new("deadb"),
            &HashMap::new(),
            10,
        );
        assert_eq!(titles(&res, &src)[0], "Unrelated change");
        // Three characters are not a SHA prefix: only the summary matches.
        let res = r.rank(
            &src,
            Scope::Commits,
            &Query::new("dea"),
            &HashMap::new(),
            10,
        );
        assert_eq!(titles(&res, &src)[0], "Add deadline handling");
        // Authors are keywords.
        let res = r.rank(
            &src,
            Scope::Commits,
            &Query::new("grace"),
            &HashMap::new(),
            10,
        );
        assert_eq!(titles(&res, &src), ["Unrelated change"]);
        let mut m = Matcher::new();
        // Keyword matches are not highlighted in the title.
        assert!(title_indices(&mut m, &Query::new("grace"), &commits[0]).is_empty());
        assert_eq!(
            title_indices(&mut m, &Query::new("unr ch"), &commits[0]),
            [0, 1, 2, 10, 11]
        );
    }

    #[test]
    fn file_names_beat_directories() {
        let files = sources::file_items(&[
            "maintenance/x.rs".into(),
            "src/main.rs".into(),
            "docs/domain/notes.md".into(),
        ]);
        let src = [(files.as_slice(), 1)];
        let mut r = Ranker::new();
        let res = r.rank(&src, Scope::Files, &Query::new("main"), &HashMap::new(), 10);
        assert_eq!(titles(&res, &src)[0], "src/main.rs");
    }

    #[test]
    fn scattered_letters_are_left_out_but_still_narrow() {
        let items = vec![
            cmd("a", "Command palette"),
            cmd("b", "Govern page: alerts, SBOM, images, protection"),
            cmd("c", "Exposure › Supply chain"),
        ];
        let src = [(items.as_slice(), 1)];
        let mut r = Ranker::new();
        let res = r.rank(
            &src,
            Scope::All,
            &Query::new("palette"),
            &HashMap::new(),
            10,
        );
        assert_eq!(titles(&res, &src), ["Command palette"]);
        assert_eq!(res.matched, 1);
        // "pa" alone is a fine match for the second; "pal" is not, but the
        // item stays a candidate, so the narrowed search equals a full one.
        let mut inc = Ranker::new();
        for q in ["p", "pa", "pal", "pale", "palet", "palett", "palette"] {
            let a = inc.rank(&src, Scope::All, &Query::new(q), &HashMap::new(), 10);
            let b = Ranker::new().rank(&src, Scope::All, &Query::new(q), &HashMap::new(), 10);
            assert_eq!(a.hits, b.hits, "{q}");
        }
    }

    #[test]
    fn empty_query_lists_scope_by_weight_frecency_and_order() {
        let items = vec![cmd("a", "A"), cmd("b", "B"), cmd("c", "C")];
        let src = [(items.as_slice(), 1)];
        let mut values = HashMap::new();
        values.insert("cmd:c".to_owned(), 1.0);
        let mut r = Ranker::new();
        let res = r.rank(&src, Scope::Commands, &Query::new(""), &values, 10);
        assert_eq!(titles(&res, &src), ["C", "A", "B"]);
        let res = r.rank(&src, Scope::Commands, &Query::new(""), &values, 2);
        assert_eq!(titles(&res, &src), ["C", "A"]);
        assert_eq!(res.matched, 3);
    }

    #[test]
    fn narrowing_gives_the_same_result_as_a_full_search() {
        let files: Vec<String> = (0..3000)
            .map(|i| format!("dir{}/sub{}/file_{}_{}.rs", i % 17, i % 5, i, i % 7))
            .collect();
        let items = sources::file_items(&files);
        let src = [(items.as_slice(), 7)];
        let mut inc = Ranker::new();
        let mut q = String::new();
        for c in "d3 file 12".chars() {
            q.push(c);
            let a = inc.rank(&src, Scope::Files, &Query::new(&q), &HashMap::new(), 50);
            let b = Ranker::new().rank(&src, Scope::Files, &Query::new(&q), &HashMap::new(), 50);
            assert_eq!(a.hits, b.hits, "query {q:?}");
            assert_eq!(a.matched, b.matched);
        }
        // A changed source is searched in full again.
        let a = inc.rank(
            &src,
            Scope::Files,
            &Query::new("d3 file 123"),
            &HashMap::new(),
            50,
        );
        assert!(a.narrowed);
        let src2 = [(items.as_slice(), 8)];
        let a = inc.rank(
            &src2,
            Scope::Files,
            &Query::new("d3 file 1234"),
            &HashMap::new(),
            50,
        );
        assert!(!a.narrowed);
        // So is another scope.
        let a = inc.rank(
            &src2,
            Scope::All,
            &Query::new("d3 file 12345"),
            &HashMap::new(),
            50,
        );
        assert!(!a.narrowed);
    }

    #[test]
    fn typed_order_and_short_file_names_break_ties() {
        let items = vec![cmd("a", "Crash fix"), cmd("b", "Fix crash")];
        let src = [(items.as_slice(), 1)];
        let mut r = Ranker::new();
        let res = r.rank(
            &src,
            Scope::All,
            &Query::new("fix crash"),
            &HashMap::new(),
            10,
        );
        assert_eq!(titles(&res, &src), ["Fix crash", "Crash fix"]);
        let files = sources::file_items(&[
            "android/res/menu/wallet_search.xml".into(),
            "components/brave_rewards/core/wallet/wallet.h".into(),
        ]);
        let src = [(files.as_slice(), 1)];
        let res = r.rank(
            &src,
            Scope::Files,
            &Query::new("wallet"),
            &HashMap::new(),
            10,
        );
        assert_eq!(
            titles(&res, &src)[0],
            "components/brave_rewards/core/wallet/wallet.h"
        );
    }

    #[test]
    fn remote_branches_match_without_the_remote() {
        let refs = sources::ref_items(&[RefEntry {
            name: "origin/wallet_meld".into(),
            full: "refs/remotes/origin/wallet_meld".into(),
            kind: Some(RefKind::Remote),
            ..Default::default()
        }]);
        let files = sources::file_items(&["android/wallet_search.xml".into()]);
        let src = [(refs.as_slice(), 1), (files.as_slice(), 1)];
        let mut r = Ranker::new();
        let res = r.rank(&src, Scope::All, &Query::new("wallet"), &HashMap::new(), 10);
        assert_eq!(titles(&res, &src)[0], "origin/wallet_meld");
    }

    #[test]
    fn keys_of_hits_are_targets() {
        let items = vec![cmd("fetch", "Fetch")];
        assert_eq!(items[0].target, Target::Command("fetch".into()));
    }

    /// `cargo test -p gitvis-core --lib palette_perf -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn palette_perf() {
        // 50,000 file paths and 50,000 commit summaries, built from a small
        // vocabulary so most single letters match most items.
        let words = [
            "main", "view", "graph", "palette", "render", "branch", "commit", "stash", "remote",
            "config", "index", "tree", "diff", "patch", "merge", "rebase", "status", "tag",
            "worker", "theme", "kit", "fuzzy", "rank", "source", "files", "search", "layout",
        ];
        let mut seed = 0x2545F4914F6CDD1Du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut pick = || words[(next() % words.len() as u64) as usize];
        let paths: Vec<String> = (0..50_000)
            .map(|i| {
                format!(
                    "crates/{}/src/{}/{}_{}_{i}.rs",
                    pick(),
                    pick(),
                    pick(),
                    pick()
                )
            })
            .collect();
        let commits: Vec<CommitEntry> = (0..50_000)
            .map(|i| CommitEntry {
                sha: format!("{:040x}", (i as u128) * 2654435761u128),
                short: format!("{:07x}", i),
                summary: format!("Fix the {} {} in {} ({})", pick(), pick(), pick(), i),
                author: pick().to_owned(),
                ..Default::default()
            })
            .collect();
        let t = Instant::now();
        let files = sources::file_items(&paths);
        let build_files = t.elapsed();
        let t = Instant::now();
        let commit_items = sources::commit_items(&commits);
        let build_commits = t.elapsed();
        println!(
            "build: 50,000 files {:?}, 50,000 commits {:?}",
            build_files, build_commits
        );
        let values = HashMap::new();
        for (label, src, scope) in [
            ("files", vec![(files.as_slice(), 1u64)], Scope::Files),
            (
                "commits",
                vec![(commit_items.as_slice(), 1u64)],
                Scope::Commits,
            ),
            (
                "everything (100,000)",
                vec![(files.as_slice(), 1u64), (commit_items.as_slice(), 1u64)],
                Scope::All,
            ),
        ] {
            println!("\n{label}:");
            let mut inc = Ranker::new();
            let mut q = String::new();
            for c in "main view rs".chars() {
                q.push(c);
                let query = Query::new(&q);
                let full = Ranker::new().rank(&src, scope, &query, &values, 500);
                let typed = inc.rank(&src, scope, &query, &values, 500);
                println!(
                    "  {q:<14} full {:>8.2} ms   typing {:>8.2} ms   {:>6} matched",
                    full.micros as f64 / 1000.0,
                    typed.micros as f64 / 1000.0,
                    full.matched
                );
            }
        }
    }
}
