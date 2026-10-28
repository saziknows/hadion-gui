//! The Packages tab: container images on GHCR, matched to commits.

use egui::Ui;

use gitvis_core::devops::runtime::packages::{
    self, DELETE_SCOPE_FIX, Listing, Package, PackageVersion, Problem, SCOPE_FIX,
};

use super::common::{self, Cx};
use super::{Act, PkgState, RefCache, State};
use crate::format;
use crate::state::Action;
use crate::theme::pal;
use crate::views::icons::{Icon, IconButton};
use crate::views::insights::{Cell, grid, list_none, list_top};
use crate::views::kit::{self, Col, Leading, Row, Stat, Tone};

/// Rows of the versions table at most.
const MAX_ROWS: usize = 50;

pub fn show(ui: &mut Ui, cx: &Cx, rt: &mut State, actions: &mut Vec<Action>) {
    let State { pkg, refs, .. } = rt;
    // The tab can open before the repository's GitHub remote is known (a
    // big repository still loading): ask once when it is.
    if cx.github && pkg.listing.is_none() && !pkg.loading && !pkg.auto_requested {
        pkg.auto_requested = true;
        actions.push(Act::PkgRefresh.into());
    }
    header(ui, cx, pkg, actions);
    if !cx.github {
        common::page(ui, "pkg_none", |ui| {
            kit::empty_state(
                ui,
                Icon::Package,
                "No GitHub remote",
                "Container packages live on GitHub (ghcr.io), and this repository has no remote on github.com. Add one to see the images built from it.",
                None,
            );
        });
        return;
    }
    let Some(listing) = &pkg.listing else {
        common::page(ui, "pkg_loading", |ui| {
            if pkg.loading {
                kit::empty_state(
                    ui,
                    Icon::Package,
                    "Loading container packages",
                    "Asking GitHub for the repository's owner, then for its container packages.",
                    None,
                );
            } else if kit::empty_state(
                ui,
                Icon::Package,
                "Container packages",
                "Lists the owner's images on the GitHub Container Registry and matches their tags to commits, tags and branches here.",
                Some((Icon::Download, "Load packages")),
            ) {
                actions.push(Act::PkgRefresh.into());
            }
        });
        return;
    };
    let listing = match listing {
        Ok(l) => l.clone(),
        Err(e) => {
            let e = e.clone();
            let owner = pkg.owner.clone();
            common::page(ui, "pkg_error", |ui| {
                error_state(ui, &e, owner.as_ref().map(|(o, _)| o.as_str()), actions)
            });
            return;
        }
    };
    if listing.packages.is_empty() {
        let owner = listing.owner.clone();
        common::page(ui, "pkg_empty", |ui| {
            kit::empty_state(
                ui,
                Icon::Package,
                &format!("{owner} has no container packages"),
                &format!(
                    "Nothing is published on ghcr.io/{} yet. Images pushed with `docker push ghcr.io/{}/NAME:TAG` (or from GitHub Actions with docker/build-push-action) show up here, matched to the commits they were built from.",
                    owner.to_lowercase(),
                    owner.to_lowercase()
                ),
                None,
            );
        });
        return;
    }
    let visible: Vec<String> = pkg.visible().iter().map(|p| p.name.clone()).collect();
    if visible.is_empty() && pkg.filter.trim().is_empty() {
        let (owner, repo, n) = (
            listing.owner.clone(),
            listing.repo.clone(),
            listing.packages.len(),
        );
        common::page(ui, "pkg_unlinked", |ui| {
            if kit::empty_state(
                ui,
                Icon::Package,
                &format!("No package is linked to {repo}"),
                &format!(
                    "{owner} has {}, none of them linked to this repository. A package is linked when its image carries the label org.opencontainers.image.source=https://github.com/{repo}, or by hand in the package settings.",
                    format::plural(n, "container package", "container packages")
                ),
                Some((Icon::List, "Show all of the owner's packages")),
            ) {
                actions.push(Act::PkgShowAll(true).into());
            }
        });
        return;
    }
    common::list_pane(ui, "pkg_list", |ui| package_list(ui, cx, pkg, actions));
    let sel = pkg.selected.clone();
    common::detail_pane(ui, ("pkg_detail", sel.clone()), |ui| {
        match sel.as_deref().and_then(|n| pkg.package(n)) {
            Some(p) => package_detail(ui, cx, pkg, refs, &listing, p, actions),
            None => {
                kit::empty_state(
                    ui,
                    Icon::Package,
                    "Select a package",
                    "Its versions, their tags and the commits they were built from appear here.",
                    None,
                );
            }
        }
    });
}

fn header(ui: &mut Ui, cx: &Cx, pkg: &mut PkgState, actions: &mut Vec<Action>) {
    let mut sub = String::from("Container images on GitHub Packages (ghcr.io), matched to commits");
    if let Some((o, kind)) = &pkg.owner {
        sub = format!("{o} ({}) · ghcr.io/{}", kind.label(), o.to_lowercase());
    }
    if let Some(Ok(l)) = &pkg.listing {
        let linked = l.packages.iter().filter(|p| p.linked_to(&l.repo)).count();
        sub = format!(
            "{} ({}) · {}, {linked} linked to {}",
            l.owner,
            l.kind.label(),
            format::plural(l.packages.len(), "package", "packages"),
            l.repo
        );
    }
    let f = common::fetched(pkg.fetched, None, cx.now);
    if !f.is_empty() {
        sub.push_str(" · ");
        sub.push_str(&f);
    }
    let show_all = pkg.show_all;
    let loading = pkg.loading;
    let has_list = matches!(&pkg.listing, Some(Ok(_)));
    crate::devops::pages::layout::page_header(ui, "Packages", &sub, |ui| {
        if loading {
            ui.add(egui::Spinner::new().size(14.0));
        }
        if IconButton::new(Icon::Rerun, "Refresh")
            .compact()
            .enabled(!loading && cx.github)
            .show(ui)
            .on_hover_text("Fetch the packages again (2 GitHub API calls)")
            .clicked()
        {
            actions.push(Act::PkgRefresh.into());
        }
        if has_list {
            let mut on = show_all;
            if crate::views::form::checkbox(ui, &mut on, "Show all of the owner's packages")
                .changed()
            {
                actions.push(Act::PkgShowAll(on).into());
            }
        }
    });
}

/// The answer was an error: the missing scope gets the full explanation
/// and the exact fix.
fn error_state(ui: &mut Ui, e: &str, owner: Option<&str>, actions: &mut Vec<Action>) {
    let problem = packages::problem(e);
    if !problem.is_scope() {
        common::error_block(
            ui,
            "GitHub did not list the packages",
            e,
            None,
            Some(("Try again", Act::PkgRefresh)),
            actions,
        );
        return;
    }
    kit::title(ui, "GitHub Packages needs one more permission");
    ui.add_space(2.0);
    let who = owner.map(|o| format!(" of {o}")).unwrap_or_default();
    kit::body(
        ui,
        &format!(
            "gitvis talks to GitHub through your GitHub CLI login (gh auth). That login can read this repository but not its container packages{who}: listing packages needs the read:packages scope, which `gh auth login` does not ask for by default.{}",
            if problem == Problem::LikelyScope {
                " GitHub answered with a 403 / 404 without naming the scope; it hides packages a token may not see, so this is almost always the cause."
            } else {
                ""
            }
        ),
    );
    kit::section(ui, "Fix it in a terminal", None);
    kit::body(
        ui,
        "Run this. It opens your browser so GitHub can ask you to approve the extra scope; your existing login and its other scopes stay as they are.",
    );
    kit::code_block(ui, SCOPE_FIX, actions);
    common::buttons(ui, |ui| {
        if IconButton::new(Icon::Copy, "Copy command")
            .primary(true)
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::Copy(SCOPE_FIX.to_owned()));
        }
        if IconButton::new(Icon::Rerun, "Check again")
            .compact()
            .show(ui)
            .on_hover_text("Ask GitHub for the packages again")
            .clicked()
        {
            actions.push(Act::PkgRefresh.into());
        }
    });
    ui.add_space(4.0);
    kit::callout(
        ui,
        Tone::Neutral,
        "Why gitvis does not run it for you",
        "It opens a browser and changes your GitHub CLI authentication, which every other tool on this machine shares. That is your call, not gitvis's.",
    );
    kit::secondary(
        ui,
        "To delete package versions from gitvis as well, grant delete:packages at the same time:",
    );
    kit::code_block(ui, DELETE_SCOPE_FIX, actions);
    kit::section(ui, "What GitHub answered", None);
    kit::code_block(ui, e, actions);
}

fn package_list(ui: &mut Ui, cx: &Cx, pkg: &mut PkgState, actions: &mut Vec<Action>) {
    let total = pkg.visible().len();
    list_top(
        ui,
        "Packages",
        total,
        false,
        Some(("pkg_filter", &mut pkg.filter, "Filter packages")),
        actions,
    );
    let vis = pkg.visible();
    if vis.is_empty() {
        list_none(ui, "No package matches the filter.");
        return;
    }
    let Some(Ok(l)) = &pkg.listing else { return };
    let p = *pal(ui);
    let rows: Vec<(String, String, String, common::Badges)> = vis
        .iter()
        .map(|x| {
            let repo = match x.repository_name() {
                Some(_) if x.linked_to(&l.repo) => "linked to this repository".to_owned(),
                Some(r) => format!("linked to {r}"),
                None => "not linked to a repository".to_owned(),
            };
            let sub = format!(
                "{} · {repo}",
                format::plural(x.version_count as usize, "version", "versions")
            );
            let meta = common::ago(x.updated(), cx.now);
            let mut badges = Vec::new();
            if !x.visibility.is_empty() {
                badges.push((
                    capitalise(&x.visibility),
                    if x.visibility == "public" {
                        Tone::Neutral
                    } else {
                        Tone::Purple
                    },
                ));
            }
            (x.name.clone(), sub, meta, badges)
        })
        .collect();
    let selected = pkg.selected.clone();
    kit::list(
        ui,
        "pkg_rows",
        rows.len(),
        kit::ROW_H,
        |i| selected.as_deref() == Some(rows[i].0.as_str()),
        |ui, rect, resp, i| {
            let (name, sub, meta, badges) = &rows[i];
            kit::paint_row(
                ui,
                rect,
                &Row::new(name)
                    .leading(Leading::Icon(Icon::Package, p.text_2))
                    .subtitle(sub)
                    .meta(meta)
                    .badges(badges),
                selected.as_deref() == Some(name.as_str()),
            );
            if resp.clicked() {
                actions.push(Act::PkgSelect(Some(name.clone())).into());
            }
            resp.context_menu(|ui| {
                if ui.button("Copy image name").clicked() {
                    actions.push(Action::Copy(packages::image(&l.owner, name)));
                    ui.close();
                }
            });
        },
    );
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn package_detail(
    ui: &mut Ui,
    cx: &Cx,
    pkg: &PkgState,
    refs: &mut RefCache,
    l: &Listing,
    p: &Package,
    actions: &mut Vec<Action>,
) {
    let mut badges = vec![(capitalise(&p.visibility), Tone::Neutral)];
    if p.linked_to(&l.repo) {
        badges.push(("Linked to this repository".into(), Tone::Accent));
    }
    crate::views::insights::detail_title(ui, &p.name, &badges);
    let image = packages::image(&l.owner, &p.name);
    kit::kv(ui, 120.0, |kv| {
        kv.text("Image", kit::mono(&image));
        match p.repository_name() {
            Some(r) => kv.text("Repository", r),
            None => kv.muted("Repository", "Not linked"),
        }
        kv.text(
            "Versions",
            if p.version_count as usize > packages::VERSIONS_PER_PAGE {
                format!(
                    "{} (the newest {} are listed)",
                    format::count(p.version_count as usize),
                    packages::VERSIONS_PER_PAGE
                )
            } else {
                format::count(p.version_count as usize)
            },
        );
        kv.text("Created", common::when(p.created(), cx.now));
        kv.text("Updated", common::when(p.updated(), cx.now));
    });
    common::buttons(ui, |ui| {
        if !p.html_url.is_empty()
            && IconButton::new(Icon::External, "Open on GitHub")
                .compact()
                .show(ui)
                .clicked()
        {
            actions.push(Action::OpenUrl(p.html_url.clone()));
        }
        if IconButton::new(Icon::Copy, "Copy image name")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::Copy(image.clone()));
        }
    });
    let state = pkg.versions.get(&p.name);
    let loading = state.is_some_and(|v| v.loading);
    kit::section_with(
        ui,
        "Versions",
        pkg.versions_of(&p.name).map(<[PackageVersion]>::len),
        |ui| {
            if loading {
                ui.add(egui::Spinner::new().size(13.0));
            }
            if IconButton::new(Icon::Rerun, "Reload")
                .compact()
                .enabled(!loading)
                .show(ui)
                .clicked()
            {
                actions.push(Act::PkgLoadVersions(p.name.clone()).into());
            }
        },
    );
    match state.and_then(|v| v.result.as_ref()) {
        None if loading => kit::loading(ui, "Loading versions…"),
        None => {
            if IconButton::new(Icon::Download, "Load versions")
                .compact()
                .show(ui)
                .clicked()
            {
                actions.push(Act::PkgLoadVersions(p.name.clone()).into());
            }
        }
        Some(Err(e)) => {
            if packages::problem(e).is_scope() {
                kit::callout_with(
                    ui,
                    Tone::Warning,
                    "Versions need the read:packages scope",
                    "Run this in a terminal (it opens your browser), then reload.",
                    |ui| kit::code_block(ui, SCOPE_FIX, actions),
                );
                kit::code_block(ui, e, actions);
            } else {
                common::error_block(
                    ui,
                    "GitHub did not list the versions",
                    e,
                    None,
                    Some(("Try again", Act::PkgLoadVersions(p.name.clone()))),
                    actions,
                );
            }
        }
        Some(Ok(vs)) if vs.is_empty() => {
            common::note(ui, "No versions: nothing was pushed to this package yet.");
        }
        Some(Ok(vs)) => versions(ui, cx, pkg, refs, l, p, vs, actions),
    }
}

#[allow(clippy::too_many_arguments)]
fn versions(
    ui: &mut Ui,
    cx: &Cx,
    pkg: &PkgState,
    refs: &mut RefCache,
    l: &Listing,
    p: &Package,
    vs: &[PackageVersion],
    actions: &mut Vec<Action>,
) {
    let matched: Vec<_> = vs
        .iter()
        .map(|v| refs.idx.as_ref().and_then(|i| i.best(v.tags())))
        .collect();
    let commits = matched
        .iter()
        .filter(|m| m.as_ref().is_some_and(|m| m.commit.is_some()))
        .count();
    let untagged = vs.iter().filter(|v| v.tags().is_empty()).count();
    let newest_commit = matched.iter().flatten().find_map(|m| m.commit);
    let newest_dist = newest_commit.map(|id| refs.distance(cx.snap, id));
    kit::stats(
        ui,
        &[
            Stat::new("versions listed", format::count(vs.len())),
            Stat::new("matched to a commit", format::count(commits)),
            Stat::new("untagged", format::count(untagged)),
            Stat::new(
                "commits the newest image is behind",
                newest_dist
                    .as_ref()
                    .and_then(|d| d.behind())
                    .map(format::count)
                    .unwrap_or_else(|| "–".into()),
            ),
        ],
    );
    let sel_id = pkg.version.or_else(|| vs.first().map(|v| v.id));
    if let Some((i, v)) = vs.iter().enumerate().find(|(_, v)| Some(v.id) == sel_id) {
        crate::views::form::card(ui, Tone::Neutral, |ui| {
            version_detail(ui, cx, refs, l, p, v, matched[i].as_ref(), actions);
        });
    }
    ui.add_space(6.0);
    let rows: Vec<Vec<Cell>> = vs
        .iter()
        .zip(&matched)
        .take(MAX_ROWS)
        .map(|(v, m)| {
            let mut tags: Vec<(String, Tone)> = v
                .tags()
                .iter()
                .take(3)
                .map(|t| (t.clone(), Tone::Neutral))
                .collect();
            if v.tags().len() > 3 {
                tags.push((format!("+{}", v.tags().len() - 3), Tone::Neutral));
            }
            if tags.is_empty() {
                tags.push(("untagged".into(), Tone::Neutral));
            }
            let digest = if Some(v.id) == sel_id {
                Cell::Tone(v.short_digest(), Tone::Accent)
            } else {
                Cell::Mono(v.short_digest())
            };
            let (commit, rel) = match m {
                Some(m) => match m.commit {
                    Some(id) => {
                        let d = refs.distance(cx.snap, id);
                        let short = gitvis_core::repo::short_id(id);
                        (
                            if cx.in_graph(id) {
                                Cell::MonoLink(short)
                            } else {
                                Cell::Mono(short)
                            },
                            Cell::Sec(match m.kind {
                                gitvis_core::devops::runtime::matching::MatchKind::Branch => {
                                    format!("branch {} tip", m.name)
                                }
                                _ => d.text(),
                            }),
                        )
                    }
                    None => (
                        Cell::Muted("–".into()),
                        Cell::Muted(common::match_text(m, None)),
                    ),
                },
                None => (Cell::Muted("–".into()), Cell::Muted("no match here".into())),
            };
            vec![
                Cell::Badges(tags),
                digest,
                commit,
                rel,
                Cell::Muted(common::ago(v.created(), cx.now)),
            ]
        })
        .collect();
    let ids: Vec<u64> = vs.iter().take(MAX_ROWS).map(|v| v.id).collect();
    grid(
        ui,
        "pkg_versions",
        &[
            Col::text("Tags"),
            Col::text("Digest"),
            Col::text("Commit"),
            Col::text("Relation"),
            Col::num("Pushed"),
        ],
        &rows,
        true,
        |r, resp| {
            if resp.clicked() {
                actions.push(Act::PkgVersion(Some(ids[r])).into());
            }
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn version_detail(
    ui: &mut Ui,
    cx: &Cx,
    refs: &mut RefCache,
    l: &Listing,
    p: &Package,
    v: &PackageVersion,
    m: Option<&gitvis_core::devops::runtime::matching::RefMatch>,
    actions: &mut Vec<Action>,
) {
    let title = v
        .tags()
        .first()
        .map(|t| format!("{}:{t}", p.name))
        .unwrap_or_else(|| format!("{}@{}", p.name, v.short_digest()));
    kit::subtitle(ui, &title);
    match m {
        Some(m) => {
            if let Some(id) = m.commit {
                let d = refs.distance(cx.snap, id);
                let rel = d.text();
                kit::body(
                    ui,
                    &format!(
                        "Image for commit {}{}",
                        gitvis_core::repo::short_id(id),
                        if rel.is_empty() {
                            String::new()
                        } else {
                            format!(": {rel}")
                        }
                    ),
                );
            }
            common::match_rows(ui, cx, refs, m, "Matched by", actions);
        }
        None => common::note(
            ui,
            if v.tags().is_empty() {
                "Untagged: only its digest identifies it (often an older build whose tags moved on, or a layer of a multi-platform image)."
            } else {
                "None of its tags names a commit, git tag or branch of this repository."
            },
        ),
    }
    kit::kv(ui, 120.0, |kv| {
        kv.row("Digest", |ui| common::copy_line(ui, v.digest(), actions));
        kv.row("Tags", |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                if v.tags().is_empty() {
                    common::note(ui, "none");
                }
                for t in v.tags() {
                    kit::badge(ui, t, Tone::Neutral);
                }
            });
        });
        kv.text("Version id", format!("{}", v.id));
        kv.text("Pushed", common::when(v.created(), cx.now));
        if v.updated() != v.created() {
            kv.text("Updated", common::when(v.updated(), cx.now));
        }
    });
    kit::section(ui, "Pull", None);
    for t in v.tags().iter().take(4) {
        common::copy_line(ui, &packages::pull_command(&l.owner, &p.name, t), actions);
    }
    common::copy_line(
        ui,
        &packages::pull_by_digest(&l.owner, &p.name, v.digest()),
        actions,
    );
    common::buttons(ui, |ui| {
        let url = if v.html_url.is_empty() {
            p.html_url.clone()
        } else {
            v.html_url.clone()
        };
        if !url.is_empty()
            && IconButton::new(Icon::External, "Open on GitHub")
                .compact()
                .show(ui)
                .clicked()
        {
            actions.push(Action::OpenUrl(url));
        }
        if crate::views::form::danger(ui, "Delete version…", true)
            .on_hover_text("Asks first; needs the delete:packages scope")
            .clicked()
        {
            actions.push(
                Act::PkgAskDelete {
                    package: p.name.clone(),
                    version: v.id,
                }
                .into(),
            );
        }
    });
}
