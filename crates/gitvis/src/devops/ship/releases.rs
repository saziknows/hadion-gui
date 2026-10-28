//! The Releases tab: the release list, a release's detail (assets,
//! downloads, notes, edit, delete) and the publish form.

use egui::{Align2, FontId, RichText, Ui, pos2, vec2};

use gitvis_core::devops::proc::render_args;
use gitvis_core::devops::ship::notes::Group;
use gitvis_core::devops::ship::release::{self, Release};

use super::{Act, EditForm, NotesFrom, ReleaseForm, State, count, ui as sui};
use crate::devops::console::Console;
use crate::devops::pages::explain;
use crate::format;
use crate::state::{Action, RepoState, Settings};
use crate::theme::{Tone, pal, size};
use crate::views::icons::{Icon, IconButton};
use crate::views::insights::{self as ins, Cell};
use crate::views::kit::{self, Col, Leading, Row};
use crate::views::{console as console_view, form};

/// One list row, computed before the split (the list pane only reads).
struct RowData {
    id: u64,
    title: String,
    subtitle: String,
    meta: String,
    badges: Vec<(String, Tone)>,
    dimmed: bool,
}

fn badges(st: &State, r: &Release) -> Vec<(String, Tone)> {
    let mut b = Vec::new();
    if st.latest_id == Some(r.id) {
        b.push(("Latest".to_owned(), Tone::Success));
    }
    if r.draft {
        b.push(("Draft".to_owned(), Tone::Neutral));
    }
    if r.prerelease {
        b.push(("Pre-release".to_owned(), Tone::Warning));
    }
    b
}

pub fn show(
    ui: &mut Ui,
    repo: &mut RepoState,
    console: &Console,
    _settings: &Settings,
    actions: &mut Vec<Action>,
) {
    let now = format::now_secs();
    let st = &repo.devops.ship;
    let subtitle = match st.releases.ok() {
        Some(list) => {
            let downloads: u64 = list.iter().map(Release::downloads).sum();
            let mut s = format!(
                "{}{} · {} downloads",
                count(list.len(), "release", "releases"),
                if st.more { " so far" } else { "" },
                format::count(downloads as usize)
            );
            let f = sui::fetched(&st.releases, now);
            if !f.is_empty() {
                s.push_str(&format!(" · {f}"));
            }
            s
        }
        None => "Published releases and their assets; publish a new one".to_owned(),
    };
    let can = repo.github_remote.is_some() && st.gh_ok != Some(false);
    crate::devops::pages::layout::page_header(ui, "Releases", &subtitle, |ui| {
        if can
            && IconButton::new(Icon::Plus, "New release")
                .primary(true)
                .compact()
                .show(ui)
                .clicked()
        {
            actions.push(Act::NewRelease.into());
        }
        if can
            && IconButton::new(Icon::Rerun, "Refresh")
                .compact()
                .enabled(!st.releases.loading)
                .show(ui)
                .on_hover_text(
                    "Read the releases again (GET releases?per_page=30 and releases/latest)",
                )
                .clicked()
        {
            actions.push(Act::RefreshReleases.into());
        }
        if st.releases.loading {
            ui.add(egui::Spinner::new().size(14.0));
        }
    });
    if !sui::gate(ui, repo, actions) {
        return;
    }
    let st = &repo.devops.ship;
    // Without any release, the form (when open) takes the whole page.
    let empty = st.releases.ok().is_some_and(|l| l.is_empty());
    if empty || st.releases.ok().is_none() {
        if st.form.open {
            let RepoState {
                devops,
                snapshot,
                github_remote,
                ..
            } = repo;
            let mut offset = devops.ship.scroll_to.take();
            sui::scroll_page(ui, "ship_release_form_page", &mut offset, |ui| {
                publish_form(
                    ui,
                    &mut devops.ship,
                    snapshot.as_ref(),
                    github_remote.as_ref(),
                    actions,
                );
            });
            return;
        }
        if empty {
            kit::scroll_page(ui, "ship_releases_empty", |ui| {
                if kit::empty_state(
                    ui,
                    Icon::Tag,
                    "No releases yet",
                    "This repository has no release on GitHub. Publish one from a tag: gitvis suggests the next version from your conventional commits and writes the notes.",
                    Some((Icon::Plus, "New release")),
                ) {
                    actions.push(Act::NewRelease.into());
                }
            });
            return;
        }
        kit::scroll_page(ui, "ship_releases_state", |ui| {
            sui::loaded(
                ui,
                &st.releases,
                "releases",
                "Loading releases",
                Act::RefreshReleases,
                actions,
            );
        });
        return;
    }

    let rows: Vec<RowData> = st
        .visible_releases()
        .into_iter()
        .map(|r| {
            let when = r
                .time()
                .map(|t| format::relative(t, now))
                .unwrap_or_else(|| "not published".to_owned());
            let mut parts: Vec<String> = Vec::new();
            if r.title() != r.tag_name {
                parts.push(r.tag_name.clone());
            }
            if !r.author_login().is_empty() {
                parts.push(r.author_login().to_owned());
            }
            parts.push(when);
            let subtitle = parts.join(" · ");
            RowData {
                id: r.id,
                title: r.title().to_owned(),
                subtitle,
                meta: if r.assets.is_empty() {
                    String::new()
                } else {
                    format!("{} downloads", format::count(r.downloads() as usize))
                },
                badges: badges(st, r),
                dimmed: r.draft,
            }
        })
        .collect();
    let selected = st.selected;
    let more = st.more;
    let loading_more = st.loading_more;
    let total = st.releases.ok().map(Vec::len).unwrap_or(0);
    let key = if st.form.open {
        "new".to_owned()
    } else {
        selected.map(|s| s.to_string()).unwrap_or_default()
    };
    let mut filter = std::mem::take(&mut repo.devops.ship.filter);
    let offset = repo.devops.ship.scroll_to.take();
    crate::devops::pages::layout::split_keyed_at(
        ui,
        "ship_releases",
        &key,
        offset,
        actions,
        |ui, actions| {
            kit::list_header(ui, "Releases", Some(total), |_| {});
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                ui.set_width(ui.available_width() - 4.0);
                kit::search_box(
                    ui,
                    "ship_release_filter",
                    &mut filter,
                    "Filter by tag, title or author",
                );
            });
            ui.add_space(6.0);
            if rows.is_empty() {
                ins::list_none(ui, "No release matches the filter.");
                return;
            }
            let n = rows.len() + usize::from(more);
            kit::list(
                ui,
                "ship_release_list",
                n,
                kit::ROW_H,
                |i| rows.get(i).is_some_and(|r| Some(r.id) == selected),
                |ui, rect, resp, i| {
                    let Some(r) = rows.get(i) else {
                        let p = *pal(ui);
                        ui.painter().text(
                            rect.center(),
                            Align2::CENTER_CENTER,
                            if loading_more {
                                "Loading more releases…".to_owned()
                            } else {
                                format!("Load {} more", release::PAGE)
                            },
                            FontId::proportional(size::SMALL),
                            p.accent,
                        );
                        if resp.clicked() && !loading_more {
                            actions.push(Act::LoadMoreReleases.into());
                        }
                        return;
                    };
                    let p = *pal(ui);
                    // In a narrow list the title needs the room more than
                    // the download count (the detail has it).
                    let meta = if rect.width() >= 340.0 {
                        r.meta.as_str()
                    } else {
                        ""
                    };
                    let row = Row::new(&r.title)
                        .leading(Leading::Icon(
                            Icon::Tag,
                            if r.dimmed { p.text_3 } else { p.text_2 },
                        ))
                        .subtitle(&r.subtitle)
                        .meta(meta)
                        .badges(&r.badges)
                        .dimmed(r.dimmed);
                    kit::paint_row(ui, rect, &row, Some(r.id) == selected);
                    if resp.clicked() {
                        actions.push(Act::SelectRelease(Some(r.id)).into());
                    }
                },
            );
        },
        |ui, actions| {
            let RepoState {
                devops,
                snapshot,
                github_remote,
                ..
            } = repo;
            let st = &mut devops.ship;
            if st.form.open {
                publish_form(ui, st, snapshot.as_ref(), github_remote.as_ref(), actions);
                return;
            }
            let Some(id) = selected.filter(|id| st.release(*id).is_some()) else {
                overview(ui, st, now, actions);
                return;
            };
            detail(ui, st, snapshot.as_ref(), id, console, now, actions);
        },
    );
    repo.devops.ship.filter = filter;
}

/// The right pane without a selection: a summary of the list.
fn overview(ui: &mut Ui, st: &State, now: i64, actions: &mut Vec<Action>) {
    let Some(list) = st.releases.ok() else { return };
    kit::title(ui, "All releases");
    let published: Vec<&Release> = list.iter().filter(|r| !r.draft).collect();
    let drafts = list.len() - published.len();
    let pre = published.iter().filter(|r| r.prerelease).count();
    let downloads: u64 = list.iter().map(Release::downloads).sum();
    let assets: usize = list.iter().map(|r| r.assets.len()).sum();
    let newest = published.iter().filter_map(|r| r.published()).max();
    let mut tiles = vec![
        kit::Stat::new("Releases loaded", format::count(list.len())),
        kit::Stat::new("Downloads", format::count(downloads as usize)),
        kit::Stat::new("Assets", format::count(assets)),
    ];
    if drafts > 0 {
        tiles.push(kit::Stat::new("Drafts", format::count(drafts)));
    }
    if pre > 0 {
        tiles.push(kit::Stat::new("Pre-releases", format::count(pre)));
    }
    if let Some(t) = newest {
        tiles.push(
            kit::Stat::new("Last published", format::relative(t, now))
                .tip("Newest published release"),
        );
    }
    let times: Vec<i64> = published.iter().filter_map(|r| r.published()).collect();
    if times.len() >= 2 {
        let span = times.iter().max().unwrap_or(&0) - times.iter().min().unwrap_or(&0);
        let days = span as f64 / 86_400.0 / (times.len() - 1) as f64;
        tiles.push(
            kit::Stat::new("Between releases", format!("{days:.0} days"))
                .tip("Mean days between the loaded published releases"),
        );
    }
    ins::stats(ui, &tiles);
    kit::section(ui, "Most downloaded", None);
    let mut top: Vec<(String, f64)> = list
        .iter()
        .filter(|r| r.downloads() > 0)
        .map(|r| (r.title().to_owned(), r.downloads() as f64))
        .collect();
    top.sort_by(|a, b| b.1.total_cmp(&a.1));
    if top.is_empty() {
        kit::muted(ui, "No asset has been downloaded yet.");
    } else {
        let c = pal(ui).accent;
        kit::bars(ui, &top, c, 10, |v| format::count(v as usize));
    }
    kit::section(ui, "Next", None);
    kit::secondary(
        ui,
        "Select a release for its assets, notes and local history.",
    );
    kit::secondary(
        ui,
        "A new release gets a suggested version from the conventional commits since the latest version tag.",
    );
    ins::button_row(ui, |ui| {
        if form::primary(ui, "New release", true).clicked() {
            actions.push(Act::NewRelease.into());
        }
    });
}

fn detail(
    ui: &mut Ui,
    st: &mut State,
    snap: Option<&gitvis_core::Snapshot>,
    id: u64,
    console: &Console,
    now: i64,
    actions: &mut Vec<Action>,
) {
    let Some(r) = st.release(id).cloned() else {
        return;
    };
    let mut b = badges(st, &r);
    if r.immutable {
        b.push(("Immutable".to_owned(), Tone::Neutral));
    }
    ins::detail_title(ui, r.title(), &b);
    let facts = st.facts.get(&r.tag_name).cloned();
    let editing = st.edit.as_ref().is_some_and(|e| e.id == id);
    ins::button_row(ui, |ui| {
        if !editing
            && IconButton::new(Icon::Edit, "Edit")
                .compact()
                .show(ui)
                .on_hover_text("Change the title, notes and flags (PATCH releases/{id})")
                .clicked()
        {
            actions.push(Act::EditRelease(id).into());
        }
        if !r.html_url.is_empty()
            && IconButton::new(Icon::External, "Open on GitHub")
                .compact()
                .show(ui)
                .clicked()
        {
            actions.push(Action::OpenUrl(r.html_url.clone()));
        }
        if facts.as_ref().is_some_and(|f| f.0.is_some())
            && IconButton::new(Icon::Graph, "Show tag in graph")
                .compact()
                .show(ui)
                .clicked()
        {
            actions.push(Act::JumpToTag(r.tag_name.clone()).into());
        }
        if IconButton::new(Icon::Trash, "Delete…")
            .compact()
            .show(ui)
            .on_hover_text("Delete the release (asks first; the tag stays)")
            .clicked()
        {
            actions.push(
                Act::DeleteRelease {
                    id,
                    with_tag: false,
                }
                .into(),
            );
        }
        if IconButton::new(Icon::Trash, "Delete with tag…")
            .compact()
            .show(ui)
            .on_hover_text("Delete the release and its tag on GitHub (asks first)")
            .clicked()
        {
            actions.push(Act::DeleteRelease { id, with_tag: true }.into());
        }
    });
    ui.add_space(4.0);
    let previous = st
        .releases
        .ok()
        .and_then(|l| l.iter().position(|x| x.id == id).map(|i| (l, i)))
        .and_then(|(l, i)| release::previous_of(l, i))
        .map(|p| p.tag_name.clone());
    kit::kv(ui, 130.0, |kv| {
        kv.row("Tag", |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(kit::mono(&r.tag_name));
                match &facts {
                    Some((Some(sha), _)) => {
                        let shown = snap.is_some_and(|s| {
                            gitvis_core::Oid::from_str(sha)
                                .ok()
                                .is_some_and(|o| s.index_of.contains_key(&o))
                        });
                        let text = format!("in local history at {}", &sha[..sha.len().min(10)]);
                        if shown {
                            if kit::link(ui, &text).clicked() {
                                actions.push(Act::JumpToTag(r.tag_name.clone()).into());
                            }
                        } else {
                            explain::quiet(ui, &format!("{text} (not in the graph)"));
                        }
                    }
                    Some((None, _)) => {
                        explain::quiet(ui, "not in local history: git fetch --tags");
                    }
                    None if st.facts_loading => {
                        explain::quiet(ui, "checking local history…");
                    }
                    None => {}
                }
            });
        });
        if !r.target_commitish.is_empty() {
            kv.text("Target", kit::mono(&r.target_commitish));
        }
        if !r.author_login().is_empty() {
            kv.text("Author", r.author_login());
        }
        match r.published() {
            Some(t) => kv.text("Published", sui::when(t, now)),
            None => kv.muted("Published", "not published (draft)"),
        }
        if let Some(c) = r.created()
            && r.published().is_none_or(|p| (p - c).abs() > 60)
        {
            kv.text("Created", sui::when(c, now));
        }
        match (&previous, facts.as_ref().and_then(|f| f.1)) {
            (Some(p), Some(n)) => kv.text(
                "Since previous",
                format!("{} since {p}", count(n as usize, "commit", "commits")),
            ),
            (Some(p), None) => kv.muted(
                "Since previous",
                &format!("unknown: {p} or {} is not in local history", r.tag_name),
            ),
            (None, _) => kv.muted("Since previous", "the oldest release loaded"),
        }
        kv.text(
            "Downloads",
            format!(
                "{} across {} ({})",
                format::count(r.downloads() as usize),
                count(r.assets.len(), "asset", "assets"),
                format::bytes(r.asset_bytes() as usize)
            ),
        );
    });

    // Assets.
    kit::section(ui, "Assets", Some(r.assets.len()));
    if r.assets.is_empty() {
        kit::secondary(
            ui,
            "No assets: GitHub still offers the source code as zip and tar.gz.",
        );
    } else {
        let cols = [
            Col::text("Name"),
            Col::num("Size"),
            Col::num("Downloads"),
            Col::text("Type"),
            Col::text("Uploaded"),
            Col::text(""),
        ];
        let rows: Vec<Vec<Cell>> = r
            .assets
            .iter()
            .map(|a| {
                vec![
                    Cell::Mono(a.name.clone()),
                    ins::sec(format::bytes(a.size as usize)),
                    ins::sec(format::count(a.download_count as usize)),
                    ins::muted(a.content_type.clone()),
                    ins::muted(
                        a.created_at
                            .as_deref()
                            .and_then(gitvis_core::github::parse_time)
                            .map(|t| format::relative(t, now))
                            .unwrap_or_default(),
                    ),
                    Cell::Tone("Download…".to_owned(), Tone::Accent),
                ]
            })
            .collect();
        ins::grid(ui, "ship_assets", &cols, &rows, true, |i, resp| {
            let a = &r.assets[i];
            if resp.clicked() {
                actions.push(
                    Act::Download {
                        tag: r.tag_name.clone(),
                        asset: a.name.clone(),
                    }
                    .into(),
                );
            }
            resp.context_menu(|ui| {
                if ui.button("Download to a folder…").clicked() {
                    actions.push(
                        Act::Download {
                            tag: r.tag_name.clone(),
                            asset: a.name.clone(),
                        }
                        .into(),
                    );
                    ui.close();
                }
                if ui.button("Copy download URL").clicked() {
                    actions.push(Action::Copy(a.browser_download_url.clone()));
                    ui.close();
                }
                if let Some(d) = &a.digest
                    && ui.button("Copy digest").clicked()
                {
                    actions.push(Action::Copy(d.clone()));
                    ui.close();
                }
            });
        });
        kit::secondary(
            ui,
            "Click an asset to download it with gh release download into a folder you pick; the output shows below.",
        );
    }
    // The newest download of this release's assets.
    let run = st
        .downloads
        .iter()
        .rev()
        .filter_map(|id| console.get(*id))
        .find(|run| run.spec.args.get(2) == Some(&r.tag_name));
    if let Some(run) = run {
        kit::section(ui, "Download", None);
        let w = ui.available_width();
        ui.allocate_ui(vec2(w, 220.0), |ui| {
            ui.set_min_height(220.0);
            console_view::output(ui, run, "ship_download", actions);
        });
    }

    // Notes, or the edit form.
    if editing {
        if let Some(e) = st.edit.as_mut() {
            edit_form(ui, e, actions);
        }
        return;
    }
    kit::section(ui, "Notes", None);
    match r.body.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        Some(body) => {
            for line in body.lines().take(400) {
                let t = line.trim_end();
                if let Some(h) = t.strip_prefix("## ").or_else(|| t.strip_prefix("### ")) {
                    ui.add_space(4.0);
                    kit::subtitle(ui, h);
                } else if t.is_empty() {
                    ui.add_space(2.0);
                } else {
                    kit::body(ui, t);
                }
            }
            if body.lines().count() > 400 {
                kit::muted(ui, "The rest of the notes is on GitHub.");
            }
        }
        None => {
            kit::muted(ui, "No notes.");
        }
    }
}

fn edit_form(ui: &mut Ui, e: &mut EditForm, actions: &mut Vec<Action>) {
    kit::section(ui, "Edit release", None);
    form::card(ui, Tone::Neutral, |ui| {
        form::field(ui, "Title", None, |ui| {
            form::text(ui, ("ship_edit_name", e.id), &mut e.name, &e.tag.clone())
        });
        form::field(ui, "Notes", Some("Markdown, as GitHub shows it."), |ui| {
            form::area(ui, ("ship_edit_body", e.id), &mut e.body, "", 14)
        });
        ui.horizontal_wrapped(|ui| {
            form::checkbox(ui, &mut e.draft, "Draft");
            ui.add_space(12.0);
            form::checkbox(ui, &mut e.prerelease, "Pre-release");
        });
        form::field(
            ui,
            "Latest",
            Some("Which release GitHub shows as latest. Drafts and pre-releases never are."),
            |ui| {
                let opts = [
                    (0u8, "Leave as is".to_owned()),
                    (1u8, "Make this the latest".to_owned()),
                    (2u8, "Not the latest".to_owned()),
                ];
                form::select(ui, ("ship_edit_latest", e.id), &mut e.latest, &opts, 220.0);
            },
        );
        form::actions_row(ui, |ui| {
            if form::primary(ui, if e.saving { "Saving…" } else { "Save…" }, !e.saving).clicked()
            {
                actions.push(Act::SaveEdit.into());
            }
            if form::button(ui, "Cancel", true).clicked() {
                actions.push(Act::CancelEdit.into());
            }
        });
    });
}

// ---------------------------------------------------------------------------
// The publish form
// ---------------------------------------------------------------------------

/// Tags for the picker: version tags first (highest version first), then
/// the rest by name, at most 60.
fn tag_options(snap: Option<&gitvis_core::Snapshot>) -> Vec<String> {
    let Some(s) = snap else { return Vec::new() };
    type Version = Option<(u64, u64, u64)>;
    let mut v: Vec<(Version, String)> = s
        .tags
        .iter()
        .map(|t| {
            (
                gitvis_core::insights::refs::parse_version(&t.name),
                t.name.clone(),
            )
        })
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    v.into_iter().map(|(_, n)| n).take(60).collect()
}

fn publish_form(
    ui: &mut Ui,
    st: &mut State,
    snap: Option<&gitvis_core::Snapshot>,
    gh: Option<&(String, String)>,
    actions: &mut Vec<Action>,
) {
    let p = *pal(ui);
    let now = format::now_secs();
    let f: &mut ReleaseForm = &mut st.form;
    kit::title(ui, "New release");
    kit::secondary(
        ui,
        "Publishes with gh release create. Nothing is sent until you confirm the exact command.",
    );

    // Suggestion.
    ui.add_space(4.0);
    match &f.prepared.result {
        Some(Ok(prep)) => {
            let s = &prep.suggestion;
            let title = match &s.base_tag {
                Some(b) => format!("Suggested {} ({} after {b})", s.next, s.bump.label()),
                None => format!("Suggested {} ({})", s.next, s.bump.label()),
            };
            let body = s.reasons.join("\n");
            let next = s.next.clone();
            let same = f.tag == next;
            kit::callout_with(ui, Tone::Accent, &title, &body, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if !same
                        && IconButton::new(Icon::Check, &format!("Use {next}"))
                            .compact()
                            .show(ui)
                            .clicked()
                    {
                        actions.push(Act::SetFormTag(next.clone()).into());
                    }
                    if IconButton::new(Icon::Rerun, "Recompute")
                        .compact()
                        .enabled(!f.prepared.loading)
                        .show(ui)
                        .on_hover_text(
                            "Read the commits between the previous tag and the target again",
                        )
                        .clicked()
                    {
                        actions.push(Act::Prepare.into());
                    }
                    if prep.truncated {
                        explain::note(
                            ui,
                            &format!(
                                "Read the newest {} commits only.",
                                gitvis_core::devops::ship::notes::MAX_COMMITS
                            ),
                        );
                    }
                });
            });
        }
        Some(Err(e)) if !f.prepared.loading => {
            kit::callout_with(ui, Tone::Warning, "No suggestion", e, |ui| {
                if IconButton::new(Icon::Rerun, "Try again")
                    .compact()
                    .show(ui)
                    .clicked()
                {
                    actions.push(Act::Prepare.into());
                }
            });
        }
        _ => kit::loading(ui, "Reading the commits since the latest version tag…"),
    }
    ui.add_space(6.0);

    let tags = tag_options(snap);
    let tag_here = snap.and_then(|s| s.tags.iter().find(|t| t.name == f.tag.trim()));
    form::card(ui, Tone::Neutral, |ui| {
        // Tag.
        let help = match tag_here {
            Some(t) => format!(
                "Existing tag at {}: the release goes on it as it is; the target is not used.",
                &t.target.to_string()[..10]
            ),
            None if f.tag.trim().is_empty() => {
                "Pick an existing tag or type a new one (e.g. v1.4.0).".to_owned()
            }
            None => format!(
                "New tag: GitHub creates {} from the target when publishing.",
                f.tag.trim()
            ),
        };
        form::field(ui, "Tag", Some(&help), |ui| {
            // The picker beside the input on a wide pane, under it on a
            // narrow one: a row wider than the pane widens the whole form
            // (and a combo box does not wrap in a wrapping row).
            let avail = ui.available_width();
            let wide = avail >= 460.0;
            let mut tag_row = |ui: &mut Ui| {
                // The frame adds its padding and border to the width.
                let w = if wide { avail - 216.0 } else { avail } - 24.0;
                form::input(
                    ui,
                    "ship_form_tag",
                    &mut f.tag,
                    form::InputOpts::new("v1.0.0").width(w).mono(),
                );
                let mut pick = String::new();
                let mut opts: Vec<(String, String)> =
                    vec![(String::new(), "Existing tags…".to_owned())];
                opts.extend(tags.iter().map(|t| (t.clone(), t.clone())));
                if form::select(ui, "ship_form_tag_pick", &mut pick, &opts, 180.0)
                    && !pick.is_empty()
                {
                    actions.push(Act::SetFormTag(pick).into());
                }
            };
            if wide {
                ui.horizontal(tag_row);
            } else {
                tag_row(ui);
            }
        });
        // Target.
        let current = snap
            .and_then(|s| s.head.branch().map(str::to_owned))
            .unwrap_or_default();
        form::field(
            ui,
            "Target",
            Some("Branch or commit the new tag is created from. Default: the current branch."),
            |ui| {
                form::input(
                    ui,
                    "ship_form_target",
                    &mut f.target,
                    form::InputOpts::new("main").mono(),
                );
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let mut chips: Vec<String> = Vec::new();
                    if !current.is_empty() {
                        chips.push(current.clone());
                    }
                    for b in ["main", "master", "trunk"] {
                        if snap.is_some_and(|s| s.branch(b).is_some())
                            && !chips.iter().any(|c| c == b)
                        {
                            chips.push(b.to_owned());
                        }
                    }
                    if let Some(h) = snap.and_then(|s| s.head.target()) {
                        chips.push(h.to_string()[..10].to_owned());
                    }
                    for c in chips {
                        if kit::chip(ui, &c, f.target.trim() == c).clicked() {
                            f.target = c;
                        }
                    }
                });
            },
        );
        // Previous tag.
        let base = f.prepared.ok().and_then(|p| p.suggestion.base_tag.clone());
        let help = match &base {
            Some(b) => format!(
                "Where the notes and the suggestion start. Empty: the latest version tag ({b})."
            ),
            None => "Where the notes and the suggestion start. Empty: the latest version tag; there is none yet, so the whole history.".to_owned(),
        };
        let hint = base.unwrap_or_else(|| "none: the whole history".to_owned());
        form::field(ui, "Previous tag", Some(&help), |ui| {
            form::input(
                ui,
                "ship_form_previous",
                &mut f.previous,
                form::InputOpts::new(&hint).mono(),
            );
        });
        if f.prepared_for != (f.target.clone(), f.previous.clone()) && !f.prepared.loading {
            ui.horizontal_wrapped(|ui| {
                explain::note(ui, "Target or previous tag changed:");
                if kit::link(ui, "recompute the suggestion").clicked() {
                    actions.push(Act::Prepare.into());
                }
            });
            ui.add_space(4.0);
        }
        // Title.
        form::field(ui, "Title", None, |ui| {
            form::text(ui, "ship_form_title", &mut f.title, f.tag.trim())
        });

        // Notes.
        let from = match f.notes_from {
            NotesFrom::Typed => "typed",
            NotesFrom::GitHub => "written by GitHub",
            NotesFrom::Local => "grouped from local commits",
        };
        form::field(ui, "Notes", Some(&format!("Markdown, {from}.")), |ui| {
            ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if IconButton::new(
                        Icon::Cloud,
                        if f.generating {
                            "Generating…"
                        } else {
                            "Generate with GitHub"
                        },
                    )
                    .compact()
                    .enabled(!f.generating && !f.tag.trim().is_empty())
                    .show(ui)
                    .on_hover_text(
                        "Lists the merged pull requests and new contributors.\nPOST releases/generate-notes: GitHub only writes text, nothing is created.",
                    )
                    .clicked()
                    {
                        actions.push(Act::GitHubNotes.into());
                    }
                    if IconButton::new(Icon::List, "Generate locally")
                        .compact()
                        .enabled(!f.prepared.loading)
                        .show(ui)
                        .on_hover_text(
                            "Groups the conventional commits since the previous tag into breaking changes, features, fixes, performance, documentation, refactoring and other.",
                        )
                        .clicked()
                    {
                        actions.push(Act::LocalNotes.into());
                    }
                    if let Some(prep) = f.prepared.ok() {
                        let parts: Vec<String> = prep
                            .groups
                            .iter()
                            .map(|(g, n)| format!("{} {n}", group_short(*g)))
                            .collect();
                        if !parts.is_empty() {
                            explain::quiet(ui, &parts.join(" · "));
                        }
                    }
                });
            if let Some(e) = &f.gen_error {
                form::error(ui, e);
            }
            ui.add_space(4.0);
            form::input(
                ui,
                "ship_form_notes",
                &mut f.notes,
                form::InputOpts::new("What changed in this release?")
                    .rows(14)
                    .mono(),
            );
            kit::muted(
                ui,
                &format!(
                    "{} · {}",
                    format::bytes(f.notes.len()),
                    count(f.notes.lines().count(), "line", "lines")
                ),
            );
        });

        // Flags.
        form::field(ui, "Options", None, |ui| {
            ui.horizontal_wrapped(|ui| {
                form::checkbox(ui, &mut f.draft, "Draft");
                ui.add_space(12.0);
                form::checkbox(ui, &mut f.prerelease, "Pre-release");
                ui.add_space(12.0);
                let mut latest = f.latest && !f.draft && !f.prerelease;
                let resp = form::checkbox(ui, &mut latest, "Make latest");
                if resp.changed() && !f.draft && !f.prerelease {
                    f.latest = latest;
                }
            });
            kit::secondary(
                ui,
                if f.draft || f.prerelease {
                    "Drafts and pre-releases are never marked latest (--latest=false)."
                } else if f.latest {
                    "GitHub marks it latest when it is the newest by date and version."
                } else {
                    "--latest=false: the current latest release keeps its badge."
                },
            );
        });

        // Assets.
        let total: u64 = f.files.iter().map(|(_, s)| *s).sum();
        form::field(
            ui,
            &format!(
                "Assets ({}, {})",
                format::count(f.files.len()),
                format::bytes(total as usize)
            ),
            Some("Files uploaded to the release after it is created."),
            |ui| {
                let mut remove = None;
                for (i, (path, size_b)) in f.files.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if IconButton::icon(Icon::Close)
                            .compact()
                            .show(ui)
                            .on_hover_text("Remove")
                            .clicked()
                        {
                            remove = Some(i);
                        }
                        ui.label(
                            RichText::new(format::bytes(*size_b as usize))
                                .size(size::SMALL)
                                .color(p.text_2),
                        );
                        // A long path is cut instead of widening the form.
                        ui.add(egui::Label::new(kit::mono(path).color(p.text)).truncate())
                            .on_hover_text(path);
                    });
                }
                if let Some(i) = remove {
                    actions.push(Act::RemoveAsset(i).into());
                }
                if IconButton::new(Icon::Plus, "Add files…")
                    .compact()
                    .show(ui)
                    .clicked()
                {
                    actions.push(Act::AddAssets.into());
                }
            },
        );

        // The command, live.
        let new = super::new_release(f);
        let full = gh.map(|(o, r)| format!("{o}/{r}")).unwrap_or_default();
        let args = release::create_args(&full, &new);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        form::field(
            ui,
            "Command",
            Some("The notes go to standard input; they never reach the command line."),
            |ui| {
                kit::code_block(ui, &format!("gh {}", render_args(&refs)), actions);
            },
        );
        if let Some(e) = &f.error {
            form::error(ui, e);
        }
        let problem = new.problem();
        form::actions_row(ui, |ui| {
            let label = if f.publishing {
                "Publishing…"
            } else if f.draft {
                "Save draft…"
            } else {
                "Publish…"
            };
            if form::primary(ui, label, problem.is_none() && !f.publishing).clicked() {
                actions.push(Act::Publish.into());
            }
            if form::button(ui, "Cancel", true).clicked() {
                actions.push(Act::CloseForm.into());
            }
            if let Some(pr) = problem {
                explain::note(ui, pr);
            }
        });
    });
    let _ = now;
    // A painted hint under the card: where the release lands.
    if let Some((o, r)) = gh {
        ui.add_space(4.0);
        let text = format!("Publishes to github.com/{o}/{r}/releases");
        let (rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), 18.0), egui::Sense::hover());
        ui.painter().text(
            pos2(rect.left(), rect.center().y),
            Align2::LEFT_CENTER,
            text,
            FontId::proportional(size::SMALL),
            p.text_3,
        );
    }
    // Version bumps and CHANGELOG.md before publishing (automation area).
    if let Some(s) = snap {
        crate::devops::automation::release_section(ui, &s.workdir, &f.tag, &f.notes, actions);
    }
}

fn group_short(g: Group) -> &'static str {
    match g {
        Group::Breaking => "breaking",
        Group::Features => "features",
        Group::Fixes => "fixes",
        Group::Performance => "performance",
        Group::Documentation => "docs",
        Group::Refactoring => "refactoring",
        Group::Other => "other",
    }
}
