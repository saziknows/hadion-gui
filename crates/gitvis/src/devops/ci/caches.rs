//! The Caches tab: Actions cache usage against the 10 GB limit, every cache
//! entry (key, ref, size, created, last used, version) or the refs that
//! hold them, delete by id or by key and ref, and bulk deletion of the
//! caches of closed pull requests.

use egui::Ui;

use gitvis_core::devops::ci::model::{CACHE_LIMIT_BYTES, Cache, CacheRef, group_by_ref, pr_number};

use super::control::{cache_prs, closed_pr_refs};
use super::widgets::{self, bytes, pct};
use super::{Act, CacheSel, CacheSort, CacheView, State};
use crate::format;
use crate::state::{Action, RepoState};
use crate::theme::{Tone, pal, space};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Col};

fn accepts(c: &Cache, needle: &str) -> bool {
    needle.is_empty()
        || c.key.to_lowercase().contains(needle)
        || c.git_ref.to_lowercase().contains(needle)
}

/// Cache entries the list shows.
pub fn visible(repo: &RepoState) -> Vec<&Cache> {
    let ci = &repo.devops.ci;
    let needle = ci.cache_filter.trim().to_lowercase();
    ci.caches
        .ok()
        .map(|l| l.items.iter().filter(|c| accepts(c, &needle)).collect())
        .unwrap_or_default()
}

/// Refs the list shows (in the By ref view).
pub fn visible_refs(repo: &RepoState) -> Vec<CacheRef> {
    let v: Vec<Cache> = visible(repo).into_iter().cloned().collect();
    group_by_ref(&v)
}

fn pr_badge(
    git_ref: &str,
    states: Option<&std::collections::HashMap<u64, String>>,
) -> Option<(String, Tone)> {
    let n = pr_number(git_ref)?;
    let s = states?.get(&n)?;
    Some(match s.as_str() {
        "OPEN" => (format!("PR #{n} open"), Tone::Neutral),
        "MERGED" => (format!("PR #{n} merged"), Tone::Warning),
        _ => (format!("PR #{n} closed"), Tone::Warning),
    })
}

pub fn show(ui: &mut Ui, repo: &mut RepoState, actions: &mut Vec<Action>) {
    let now = format::now_secs();
    let prs = cache_prs(repo);
    let closed = closed_pr_refs(repo);
    let State {
        caches,
        cache_sort,
        cache_filter,
        cache_view,
        cache_selected,
        pr_states,
        cache_busy,
        ..
    } = &mut repo.devops.ci;
    let key = match cache_selected {
        Some(CacheSel::Entry(id)) => id.to_string(),
        Some(CacheSel::Ref(r)) => r.clone(),
        None => String::new(),
    };
    let selected = cache_selected.clone();
    let items: Vec<Cache> = caches.ok().map(|l| l.items.clone()).unwrap_or_default();
    let usage = caches.ok().and_then(|l| l.usage.clone());
    let total = caches.ok().map(|l| l.total).unwrap_or(0);
    let load = &*caches;
    let states = pr_states.ok();
    let pr_load = &*pr_states;
    let busy = *cache_busy;
    let view = *cache_view;
    let sort = *cache_sort;
    kit::split_keyed(
        ui,
        "ci_caches",
        &key,
        actions,
        |ui, actions| {
            let hint = format!("Filter {} caches by key or ref", items.len());
            ui.horizontal(|ui| {
                if !cache_filter.is_empty() {
                    ui.set_max_width(ui.available_width() - 8.0);
                }
                kit::search_box(ui, "ci_cache_filter", cache_filter, &hint);
            });
            ui.add_space(space::S);
            ui.horizontal(|ui| {
                ui.add_space(2.0);
                if let Some(v) = kit::segmented(
                    ui,
                    &[(CacheView::Entries, "Entries"), (CacheView::Refs, "By ref")],
                    view,
                ) {
                    actions.push(Act::SetCacheView(v).into());
                }
                if view == CacheView::Entries {
                    ui.add_space(6.0);
                    if let Some(s) = kit::segmented(
                        ui,
                        &[
                            (CacheSort::LastUsed, CacheSort::LastUsed.label()),
                            (CacheSort::Size, CacheSort::Size.label()),
                            (CacheSort::Created, CacheSort::Created.label()),
                        ],
                        sort,
                    ) {
                        actions.push(Act::SetCacheSort(s).into());
                    }
                }
            });
            ui.add_space(space::S);
            if load.ok().is_none() {
                widgets::list_state(ui, load, "the caches", "Loading the caches…");
                return;
            }
            if let Some(l) = load.ok() {
                let order = match l.sort {
                    CacheSort::LastUsed => "most recently used first",
                    CacheSort::Size => "largest first",
                    CacheSort::Created => "newest first",
                };
                let refs = group_by_ref(&l.items).len();
                widgets::note(
                    ui,
                    &if load.loading {
                        "Sorting again…".to_owned()
                    } else if view == CacheView::Refs {
                        format!(
                            "{} holding {}, largest first",
                            format::plural(refs, "ref", "refs"),
                            format::plural(l.items.len(), "cache", "caches")
                        )
                    } else if (l.total as usize) > l.items.len() {
                        format!(
                            "{} of {} entries, {order}",
                            format::count(l.items.len()),
                            format::count(l.total as usize)
                        )
                    } else {
                        format!("{} entries, {order}", format::count(l.items.len()))
                    },
                );
                ui.add_space(4.0);
            }
            if items.is_empty() {
                kit::muted(
                    ui,
                    "This repository has no Actions caches. Workflows create them with actions/cache or the cache option of setup actions (setup-node, setup-python, …).",
                );
                return;
            }
            let needle = cache_filter.trim().to_lowercase();
            let shown: Vec<&Cache> = items.iter().filter(|c| accepts(c, &needle)).collect();
            if shown.is_empty() {
                kit::muted(ui, "No caches match the filter.");
                return;
            }
            match view {
                CacheView::Entries => {
                    kit::list(
                        ui,
                        "ci_cache_list",
                        shown.len(),
                        kit::ROW_H,
                        |i| selected == Some(CacheSel::Entry(shown[i].id)),
                        |ui, rect, resp, i| {
                            let c = shown[i];
                            let sub = format!(
                                "{}  ·  used {}",
                                c.git_ref,
                                c.last_accessed()
                                    .map(|t| format::relative(t, now))
                                    .unwrap_or_else(|| "never".to_owned())
                            );
                            let badges: Vec<(String, Tone)> =
                                pr_badge(&c.git_ref, states).into_iter().collect();
                            kit::paint_row(
                                ui,
                                rect,
                                &kit::Row::new(&c.key)
                                    .subtitle(&sub)
                                    .meta(&bytes(c.size_in_bytes))
                                    .badges(&badges),
                                selected == Some(CacheSel::Entry(c.id)),
                            );
                            if resp.clicked() {
                                actions.push(Act::SelectCache(Some(CacheSel::Entry(c.id))).into());
                            }
                        },
                    );
                }
                CacheView::Refs => {
                    let owned: Vec<Cache> = shown.iter().map(|c| (*c).clone()).collect();
                    let refs = group_by_ref(&owned);
                    kit::list(
                        ui,
                        "ci_cache_refs",
                        refs.len(),
                        kit::ROW_H,
                        |i| selected.as_ref() == Some(&CacheSel::Ref(refs[i].git_ref.clone())),
                        |ui, rect, resp, i| {
                            let r = &refs[i];
                            let sub = format!(
                                "{}  ·  used {}",
                                format::plural(r.count, "cache", "caches"),
                                r.last_accessed
                                    .map(|t| format::relative(t, now))
                                    .unwrap_or_else(|| "never".to_owned())
                            );
                            let badges: Vec<(String, Tone)> =
                                pr_badge(&r.git_ref, states).into_iter().collect();
                            let sel = CacheSel::Ref(r.git_ref.clone());
                            kit::paint_row(
                                ui,
                                rect,
                                &kit::Row::new(&r.git_ref)
                                    .subtitle(&sub)
                                    .meta(&bytes(r.bytes))
                                    .badges(&badges),
                                selected.as_ref() == Some(&sel),
                            );
                            if resp.clicked() {
                                actions.push(Act::SelectCache(Some(sel)).into());
                            }
                        },
                    );
                }
            }
        },
        |ui, actions| {
            widgets::fit_detail(ui);
            if load.ok().is_none() {
                kit::title(ui, "Caches");
                widgets::loaded(
                    ui,
                    load,
                    "the caches",
                    "Loading the caches…",
                    Act::Refresh(super::Tab::Caches).into(),
                    actions,
                );
                return;
            }
            match &selected {
                None => overview(
                    ui,
                    &items,
                    total,
                    usage.as_ref(),
                    &prs,
                    &closed,
                    pr_load,
                    busy,
                    now,
                    actions,
                ),
                Some(CacheSel::Entry(id)) => match items.iter().find(|c| c.id == *id) {
                    Some(c) => entry(ui, c, &items, states, busy, now, actions),
                    None => {
                        kit::muted(ui, "This cache is gone.");
                    }
                },
                Some(CacheSel::Ref(r)) => ref_detail(ui, r, &items, states, busy, now, actions),
            }
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn overview(
    ui: &mut Ui,
    items: &[Cache],
    total: u64,
    usage: Option<&gitvis_core::devops::ci::model::CacheUsage>,
    prs: &[u64],
    closed: &[String],
    pr_load: &super::Load<std::collections::HashMap<u64, String>>,
    busy: bool,
    now: i64,
    actions: &mut Vec<Action>,
) {
    kit::title(ui, "Caches");
    kit::secondary(ui, "Dependency and build caches saved by actions/cache.");
    let used = usage
        .map(|u| u.active_caches_size_in_bytes)
        .unwrap_or_else(|| items.iter().map(|c| c.size_in_bytes).sum());
    let count = usage.map(|u| u.active_caches_count).unwrap_or(total);
    let refs = group_by_ref(items);
    let frac = used as f64 / CACHE_LIMIT_BYTES as f64;
    kit::stats(
        ui,
        &[
            kit::Stat::new("Caches", format::count(count as usize)),
            kit::Stat::new("Used of 10 GB", bytes(used)).tone_if(frac > 0.9, Tone::Warning),
            kit::Stat::new("Refs", format::count(refs.len())),
            kit::Stat::new("Pull request refs", format::count(prs.len())),
        ],
    );
    widgets::meter(
        ui,
        frac as f32,
        &format!("{} of the 10 GB limit ({})", bytes(used), pct(frac)),
        if frac > 0.9 {
            Tone::Warning
        } else {
            Tone::Neutral
        },
    );
    crate::views::github::details_section(
        ui,
        "ci_cache_rules",
        "How GitHub keeps caches",
        false,
        |ui| {
            kit::kv(ui, 120.0, |kv| {
                kv.text("Limit", "10 GB per repository");
                kv.text(
                    "Over the limit",
                    "The least recently used caches are evicted first, so stale caches push out useful ones.",
                );
                kv.text(
                    "Pull requests",
                    "A cache saved on a pull request's ref is readable only by that pull request's runs.",
                );
            });
        },
    );
    if (total as usize) > items.len() {
        kit::muted(
            ui,
            &format!(
                "{} of {} entries are listed (5 pages of 100).",
                items.len(),
                format::count(total as usize)
            ),
        );
    }
    kit::section(ui, "Largest refs", Some(refs.len().min(10)));
    let bars: Vec<(String, f64)> = refs
        .iter()
        .map(|r| (format!("{} ({})", r.git_ref, r.count), r.bytes as f64))
        .collect();
    kit::bars(ui, &bars, pal(ui).accent, 10, |v| bytes(v as u64));

    kit::section(ui, "Caches of closed pull requests", None);
    if prs.is_empty() {
        kit::muted(ui, "No cache belongs to a pull request ref.");
        return;
    }
    match pr_load.ok() {
        None => {
            kit::secondary(
                ui,
                &format!(
                    "{} hold caches. Ask GitHub which of them are closed or merged (one GraphQL request per 100 pull requests); their caches can never be used again.",
                    format::plural(prs.len(), "pull request", "pull requests")
                ),
            );
            if let Some(e) = pr_load.err() {
                kit::callout(
                    ui,
                    Tone::Danger,
                    "Could not read the pull request states",
                    e,
                );
            }
            if pr_load.loading {
                kit::loading(ui, "Asking GitHub…");
            } else if IconButton::new(
                Icon::PullRequest,
                &format!(
                    "Check {} ({} API {})",
                    format::plural(prs.len(), "pull request", "pull requests"),
                    prs.len().div_ceil(100),
                    if prs.len() <= 100 { "call" } else { "calls" }
                ),
            )
            .compact()
            .show(ui)
            .clicked()
            {
                actions.push(Act::CheckPrStates.into());
            }
        }
        Some(states) => {
            let open = prs
                .iter()
                .filter(|n| states.get(n).is_some_and(|s| s == "OPEN"))
                .count();
            let gone: Vec<&Cache> = items
                .iter()
                .filter(|c| closed.contains(&c.git_ref))
                .collect();
            let size: u64 = gone.iter().map(|c| c.size_in_bytes).sum();
            kit::stats(
                ui,
                &[
                    kit::Stat::new("Open", format::count(open)),
                    kit::Stat::new("Closed or merged", format::count(prs.len() - open))
                        .tone_if(prs.len() > open, Tone::Warning),
                    kit::Stat::new("Their caches", format::count(gone.len())),
                    kit::Stat::new("Their size", bytes(size)),
                ],
            );
            if gone.is_empty() {
                kit::muted(ui, "Every pull request with caches is still open.");
            } else {
                let rows: Vec<Vec<String>> =
                    group_by_ref(&gone.iter().map(|c| (*c).clone()).collect::<Vec<_>>())
                        .into_iter()
                        .take(12)
                        .map(|r| {
                            vec![
                                r.git_ref.clone(),
                                format::count(r.count),
                                bytes(r.bytes),
                                r.last_accessed
                                    .map(|t| format::relative(t, now))
                                    .unwrap_or_default(),
                            ]
                        })
                        .collect();
                kit::table(
                    ui,
                    &[
                        Col::text("Ref"),
                        Col::num("Caches"),
                        Col::num("Size"),
                        Col::num("Last used"),
                    ],
                    &rows,
                );
                ui.add_space(space::S);
                if busy {
                    kit::loading(ui, "Deleting…");
                } else if crate::views::form::danger(
                    ui,
                    &format!(
                        "Delete {} of closed pull requests ({})…",
                        format::plural(gone.len(), "cache", "caches"),
                        bytes(size)
                    ),
                    true,
                )
                .clicked()
                {
                    actions.push(Act::AskDeleteClosedPrCaches.into());
                }
            }
        }
    }
}

fn entry(
    ui: &mut Ui,
    c: &Cache,
    items: &[Cache],
    states: Option<&std::collections::HashMap<u64, String>>,
    busy: bool,
    now: i64,
    actions: &mut Vec<Action>,
) {
    widgets::header(ui, &c.key, |ui| {
        if busy {
            ui.add(egui::Spinner::new().size(14.0));
        } else if IconButton::new(Icon::Trash, "Delete")
            .compact()
            .danger()
            .show(ui)
            .on_hover_text("DELETE actions/caches/{id}")
            .clicked()
        {
            actions.push(Act::AskDeleteCache(c.id).into());
        }
    });
    let mut b = vec![(bytes(c.size_in_bytes), Tone::Neutral)];
    b.extend(pr_badge(&c.git_ref, states));
    widgets::badges(ui, &b);
    kit::kv(ui, 92.0, |kv| {
        kv.text("Key", kit::mono(&c.key));
        kv.row("Ref", |ui| {
            if kit::link(ui, &c.git_ref)
                .on_hover_text("Show every cache of this ref")
                .clicked()
            {
                actions.push(Act::SetCacheView(CacheView::Refs).into());
                actions.push(Act::SelectCache(Some(CacheSel::Ref(c.git_ref.clone()))).into());
            }
        });
        kv.text("Version", kit::mono(&c.version));
        kv.text(
            "Size",
            format!(
                "{}  ({} bytes)",
                bytes(c.size_in_bytes),
                format::count(c.size_in_bytes as usize)
            ),
        );
        match c.created() {
            Some(t) => kv.text("Created", widgets::when(t, now)),
            None => kv.muted("Created", "unknown"),
        }
        match c.last_accessed() {
            Some(t) => kv.text("Last used", widgets::when(t, now)),
            None => kv.muted("Last used", "never"),
        }
        kv.text("Cache id", kit::mono(c.id.to_string()));
    });
    let same: Vec<&Cache> = items
        .iter()
        .filter(|x| x.key == c.key && x.git_ref == c.git_ref)
        .collect();
    kit::section(
        ui,
        "Every version of this key on this ref",
        Some(same.len()),
    );
    kit::secondary(
        ui,
        "A key can have several versions (one per set of cached paths and compression). Deleting by key and ref removes all of them in one request.",
    );
    ui.add_space(4.0);
    if !busy
        && crate::views::form::danger(
            ui,
            &format!(
                "Delete {} with this key on {}…",
                format::plural(same.len(), "cache", "caches"),
                c.git_ref
            ),
            true,
        )
        .clicked()
    {
        actions.push(
            Act::AskDeleteCacheKey {
                key: c.key.clone(),
                git_ref: c.git_ref.clone(),
            }
            .into(),
        );
    }
}

fn ref_detail(
    ui: &mut Ui,
    git_ref: &str,
    items: &[Cache],
    states: Option<&std::collections::HashMap<u64, String>>,
    busy: bool,
    now: i64,
    actions: &mut Vec<Action>,
) {
    let mine: Vec<&Cache> = items.iter().filter(|c| c.git_ref == git_ref).collect();
    let size: u64 = mine.iter().map(|c| c.size_in_bytes).sum();
    widgets::header(ui, git_ref, |_| {});
    let mut b = vec![(
        format!(
            "{}, {}",
            format::plural(mine.len(), "cache", "caches"),
            bytes(size)
        ),
        Tone::Neutral,
    )];
    b.extend(pr_badge(git_ref, states));
    widgets::badges(ui, &b);
    if pr_number(git_ref).is_some() {
        kit::secondary(
            ui,
            "A pull request's caches are only readable by runs of that pull request; once it is closed they only take space until GitHub evicts them.",
        );
    }
    let rows: Vec<Vec<String>> = mine
        .iter()
        .map(|c| {
            vec![
                c.key.clone(),
                bytes(c.size_in_bytes),
                c.created()
                    .map(|t| format::relative(t, now))
                    .unwrap_or_default(),
                c.last_accessed()
                    .map(|t| format::relative(t, now))
                    .unwrap_or_default(),
            ]
        })
        .collect();
    kit::section(ui, "Caches", Some(mine.len()));
    kit::table(
        ui,
        &[
            Col::text("Key"),
            Col::num("Size"),
            Col::num("Created"),
            Col::num("Last used"),
        ],
        &rows,
    );
    ui.add_space(space::M);
    if busy {
        kit::loading(ui, "Deleting…");
    } else if !mine.is_empty()
        && crate::views::form::danger(
            ui,
            &format!(
                "Delete all {} of this ref ({})…",
                format::plural(mine.len(), "cache", "caches"),
                bytes(size)
            ),
            true,
        )
        .clicked()
    {
        actions.push(Act::AskDeleteRef(git_ref.to_owned()).into());
    }
}
