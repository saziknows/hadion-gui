//! Supply chain: the SBOM (lockfile packages, exports, GitHub's dependency
//! graph) and container images (Dockerfile lint, compose images, scans).

use std::collections::{BTreeMap, BTreeSet};

use egui::{RichText, Ui};

use gitvis_core::devops::govern::alerts::Severity;
use gitvis_core::devops::govern::docker::{Dockerfile, Finding, ImagesReport, Scanner};
use gitvis_core::devops::govern::sbom::{self, GhSbom, LocalSbom};
use gitvis_core::devops::tools::Tool;
use gitvis_core::osint::Package;

use super::widgets::{self, severity_badge, severity_dot};
use super::{Act, ScanFailure, ScanRun, ScannerStatus, State};
use crate::devops::console::Console;
use crate::format;
use crate::state::{Action, RepoState};
use crate::theme::{pal, size};
use crate::views::form;
use crate::views::icons::{Icon, IconButton};
use crate::views::insights::{Cell, grid, mono, muted, num, sec, text};
use crate::views::kit::{self, Col, Leading, Row, Tone};

// ---------------------------------------------------------------------------
// SBOM
// ---------------------------------------------------------------------------

/// `ecosystem|name|version`.
pub fn package_key(p: &Package) -> String {
    format!("{}|{}|{}", p.ecosystem, p.name, p.version)
}

/// Distinct packages (the list is sorted, so duplicates across lockfiles
/// are adjacent) with the number of lockfiles each appears in.
fn groups<'a>(packages: &'a [Package], q: &str, eco: Option<&str>) -> Vec<(&'a Package, usize)> {
    let q = q.trim().to_lowercase();
    let mut out: Vec<(&Package, usize)> = Vec::new();
    for p in packages {
        if let Some(e) = eco
            && p.ecosystem != e
        {
            continue;
        }
        if !q.is_empty() && !p.name.to_lowercase().contains(&q) && !p.version.contains(&q) {
            continue;
        }
        match out.last_mut() {
            Some((last, n))
                if last.ecosystem == p.ecosystem
                    && last.name == p.name
                    && last.version == p.version =>
            {
                *n += 1
            }
            _ => out.push((p, 1)),
        }
    }
    out
}

/// Packages the list shows (for `gov-item N`).
pub fn visible_packages(st: &State) -> Vec<&Package> {
    st.sbom
        .ok()
        .map(|s| {
            groups(&s.packages, &st.sbom_search, st.sbom_eco.as_deref())
                .into_iter()
                .map(|(p, _)| p)
                .collect()
        })
        .unwrap_or_default()
}

pub fn sbom(ui: &mut Ui, repo: &mut RepoState, actions: &mut Vec<Action>) {
    let has_gh = repo.github_remote.is_some();
    let State {
        sbom,
        gh_sbom,
        sbom_search,
        sbom_eco,
        sbom_selected,
        ..
    } = &mut repo.devops.govern;
    let ready = sbom.ok().is_some();
    crate::devops::pages::layout::page_header(
        ui,
        "SBOM",
        "Every locked package in the working tree, as CycloneDX 1.5 or SPDX 2.3, compared with GitHub's dependency graph",
        |ui| {
            widgets::refresh(
                ui,
                sbom.loading,
                "Read the lockfiles again (local)",
                actions,
            );
            if IconButton::new(Icon::Download, "SPDX 2.3")
                .compact()
                .enabled(ready)
                .show(ui)
                .on_hover_text("Save an SPDX 2.3 JSON document")
                .clicked()
            {
                actions.push(Act::Export("spdx".into()).into());
            }
            if IconButton::new(Icon::Download, "CycloneDX 1.5")
                .compact()
                .enabled(ready)
                .show(ui)
                .on_hover_text("Save a CycloneDX 1.5 JSON document")
                .clicked()
            {
                actions.push(Act::Export("cyclonedx".into()).into());
            }
            ui.add_space(6.0);
            widgets::fetched(ui, sbom, "");
        },
    );
    let Some(local) = sbom.ok() else {
        let error = sbom.error.clone();
        kit::scroll_page(ui, "gov_sbom_wait", |ui| match &error {
            Some(e) => widgets::failed(
                ui,
                "Could not list the tracked files",
                e,
                Act::LoadSbom,
                actions,
            ),
            None => widgets::loading(ui, "Reading the lockfiles…"),
        });
        return;
    };
    let ecos = local.by_ecosystem();
    let visible = groups(&local.packages, sbom_search, sbom_eco.as_deref());
    let sel = sbom_selected.clone();
    let gh = gh_sbom.ok();
    crate::devops::pages::layout::split_keyed(
        ui,
        "gov_sbom",
        sel.as_deref().unwrap_or(""),
        actions,
        |ui, actions| {
            widgets::list_head(
                ui,
                "Packages",
                visible.len(),
                sel.is_some(),
                Act::SelectPackage(None),
                actions,
            );
            widgets::list_filter(ui, "gov_sbom_search", sbom_search, "Name or version");
            if ecos.len() > 1 {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(4.0);
                    ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                    if kit::chip(ui, "All", sbom_eco.is_none()).clicked() {
                        *sbom_eco = None;
                    }
                    for (e, n) in &ecos {
                        let on = sbom_eco.as_deref() == Some(*e);
                        if kit::chip(ui, &format!("{e} {n}"), on).clicked() {
                            *sbom_eco = if on { None } else { Some((*e).to_owned()) };
                        }
                    }
                });
                ui.add_space(6.0);
            }
            if visible.is_empty() {
                widgets::list_none(
                    ui,
                    if local.packages.is_empty() {
                        "No locked packages: no package-lock.json, yarn.lock, pnpm-lock.yaml, Cargo.lock, go.mod, Pipfile.lock, requirements*.txt, Gemfile.lock or composer.lock is tracked."
                    } else {
                        "Nothing matches the filter."
                    },
                );
                return;
            }
            kit::list(
                ui,
                "gov_sbom_list",
                visible.len(),
                kit::ROW_H,
                |i| sel.as_deref() == Some(package_key(visible[i].0).as_str()),
                |ui, rect, resp, i| {
                    let (p, n) = visible[i];
                    let key = package_key(p);
                    let sub = if n > 1 {
                        format!("{} · {} lockfiles", p.ecosystem, n)
                    } else {
                        format!("{} · {}", p.ecosystem, p.file)
                    };
                    kit::paint_row(
                        ui,
                        rect,
                        &Row::new(&p.name).subtitle(&sub).meta(&p.version),
                        sel.as_deref() == Some(key.as_str()),
                    );
                    if resp.clicked() {
                        actions.push(Act::SelectPackage(Some(key)).into());
                    }
                },
            );
        },
        |ui, actions| {
            let picked = sel
                .as_deref()
                .and_then(|k| local.packages.iter().find(|p| package_key(p) == k));
            match picked {
                Some(p) => package_detail(ui, local, gh, p, actions),
                None => sbom_overview(ui, local, gh_sbom, has_gh, actions),
            }
        },
    );
}

fn package_detail(
    ui: &mut Ui,
    local: &LocalSbom,
    gh: Option<&GhSbom>,
    p: &Package,
    actions: &mut Vec<Action>,
) {
    kit::title(ui, &p.name);
    ui.horizontal(|ui| {
        kit::badge(ui, p.ecosystem, Tone::Neutral);
        kit::badge(ui, &p.version, Tone::Neutral);
    });
    let purl = sbom::purl(p);
    let files: Vec<&str> = local
        .packages
        .iter()
        .filter(|q| q.ecosystem == p.ecosystem && q.name == p.name && q.version == p.version)
        .map(|q| q.file.as_str())
        .collect();
    let others: BTreeSet<&str> = local
        .packages
        .iter()
        .filter(|q| q.ecosystem == p.ecosystem && q.name == p.name && q.version != p.version)
        .map(|q| q.version.as_str())
        .collect();
    ui.add_space(4.0);
    kit::kv(ui, 130.0, |kv| {
        kv.text("Package URL", kit::mono(&purl));
        kv.text("Locked in", files.join(", "));
        if others.is_empty() {
            kv.muted("Other versions", "none: one version in the tree");
        } else {
            kv.text(
                "Other versions",
                others.iter().copied().collect::<Vec<_>>().join(", "),
            );
        }
        if let Some(g) = gh {
            let t = sbom::purl_type(p.ecosystem);
            let on_gh: Vec<String> = g
                .packages
                .iter()
                .filter_map(|x| x.purl.as_deref().and_then(sbom::parse_purl))
                .filter(|(pt, n, _)| pt == t && n.eq_ignore_ascii_case(&p.name))
                .map(|(_, _, v)| v)
                .collect();
            if on_gh.is_empty() {
                kv.muted("On GitHub", "not in GitHub's dependency graph");
            } else {
                kv.text("On GitHub", on_gh.join(", "));
            }
        }
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if IconButton::new(Icon::Copy, "Copy purl")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::Copy(purl.clone()));
        }
        let registry = match p.ecosystem {
            "npm" => Some(format!(
                "https://www.npmjs.com/package/{}/v/{}",
                p.name, p.version
            )),
            "crates.io" => Some(format!("https://crates.io/crates/{}/{}", p.name, p.version)),
            "PyPI" => Some(format!(
                "https://pypi.org/project/{}/{}/",
                p.name, p.version
            )),
            "Go" => Some(format!(
                "https://pkg.go.dev/{}@v{}",
                p.name,
                p.version.trim_start_matches('v')
            )),
            "RubyGems" => Some(format!(
                "https://rubygems.org/gems/{}/versions/{}",
                p.name, p.version
            )),
            "Packagist" => Some(format!(
                "https://packagist.org/packages/{}#{}",
                p.name, p.version
            )),
            _ => None,
        };
        if let Some(url) = registry
            && IconButton::new(Icon::External, "Registry page")
                .compact()
                .show(ui)
                .clicked()
        {
            actions.push(Action::OpenUrl(url));
        }
        let osv = format!(
            "https://osv.dev/list?ecosystem={}&q={}",
            p.ecosystem, p.name
        );
        if IconButton::new(Icon::Shield, "Advisories on osv.dev")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::OpenUrl(osv));
        }
    });
}

fn sbom_overview(
    ui: &mut Ui,
    local: &LocalSbom,
    gh: &super::Fetch<GhSbom>,
    has_gh: bool,
    actions: &mut Vec<Action>,
) {
    let unique = local.unique().len();
    let ecos = local.by_ecosystem();
    crate::views::insights::stats(
        ui,
        &[
            kit::Stat::new("Packages", format::count(unique))
                .tip("Distinct ecosystem, name and version"),
            kit::Stat::new("Entries", format::count(local.packages.len()))
                .tip("Package entries across all lockfiles (one package can be locked in several)"),
            kit::Stat::new("Ecosystems", ecos.len().to_string()),
            kit::Stat::new("Lockfiles", local.lockfiles.len().to_string()),
            kit::Stat::new("Git dependencies", local.git_deps.len().to_string())
                .tone_if(!local.git_deps.is_empty(), Tone::Warning)
                .tip("Pulled straight from a git repository: no registry version, not in the SBOM"),
            kit::Stat::new("Read", format::millis(local.elapsed_ms))
                .tip("Time to read and parse the manifests"),
        ],
    );
    kit::secondary(
        ui,
        &format!(
            "{} of manifests and lockfiles read from the working tree (vendored copies under node_modules/, vendor/ and third_party/ skipped). Exports list each distinct package once with its package URL and the lockfiles it came from.",
            format::bytes(local.bytes_read)
        ),
    );
    if !ecos.is_empty() {
        kit::section(ui, "By ecosystem", Some(ecos.len()));
        let items: Vec<(String, u32)> = ecos
            .iter()
            .map(|(e, n)| ((*e).to_owned(), *n as u32))
            .collect();
        kit::count_bars(ui, &items, pal(ui).accent, 10);
    }
    kit::section(ui, "Lockfiles and manifests", Some(local.lockfiles.len()));
    if local.lockfiles.is_empty() {
        kit::muted(
            ui,
            "None tracked. gitvis reads package-lock.json, npm-shrinkwrap.json, yarn.lock, pnpm-lock.yaml, Cargo.lock, go.mod, Pipfile.lock, requirements*.txt, Gemfile.lock and composer.lock.",
        );
    } else {
        let mut rows: Vec<&(String, usize)> = local.lockfiles.iter().collect();
        rows.sort_by_key(|a| std::cmp::Reverse(a.1));
        let cells: Vec<Vec<Cell>> = rows
            .iter()
            .take(60)
            .map(|(f, n)| vec![mono(f.as_str()), num(*n)])
            .collect();
        grid(
            ui,
            "gov_sbom_files",
            &[Col::text("File"), Col::num("Packages")],
            &cells,
            false,
            |_, _| {},
        );
    }
    if !local.git_deps.is_empty() {
        kit::section(ui, "Dependencies from git", Some(local.git_deps.len()));
        let cells: Vec<Vec<Cell>> = local
            .git_deps
            .iter()
            .take(40)
            .map(|g| {
                vec![
                    text(g.name.as_str()),
                    mono(g.source.as_str()),
                    sec(if g.line > 0 {
                        format!("{}:{}", g.file, g.line)
                    } else {
                        g.file.clone()
                    }),
                ]
            })
            .collect();
        grid(
            ui,
            "gov_sbom_git",
            &[Col::text("Name"), Col::text("Source"), Col::text("Where")],
            &cells,
            false,
            |_, _| {},
        );
    }

    kit::section(ui, "GitHub's dependency graph", None);
    if !has_gh {
        kit::muted(ui, "No GitHub remote: nothing to compare with.");
        return;
    }
    if let Some(e) = &gh.error {
        kit::callout_with(
            ui,
            if gitvis_core::devops::govern::http_status(e) == Some(404) {
                Tone::Warning
            } else {
                Tone::Danger
            },
            "GitHub has no SBOM for this repository",
            "The export reads GitHub's dependency graph, which is off by default on private repositories (Settings → Code security → Dependency graph).",
            |ui| {
                kit::code_block(ui, e, actions);
                if IconButton::new(Icon::Rerun, "Try again")
                    .compact()
                    .show(ui)
                    .clicked()
                {
                    actions.push(Act::FetchGhSbom.into());
                }
            },
        );
        return;
    }
    let Some(g) = gh.ok() else {
        if gh.loading {
            kit::loading(ui, "Exporting the SBOM from GitHub…");
        } else {
            kit::body(
                ui,
                "GitHub builds its own SBOM from the manifests it understands. Comparing shows packages only one side knows and version differences (one request: gh api repos/OWNER/REPO/dependency-graph/sbom).",
            );
            if IconButton::new(Icon::Download, "Fetch and compare")
                .primary(true)
                .show(ui)
                .clicked()
            {
                actions.push(Act::FetchGhSbom.into());
            }
        }
        return;
    };
    let d = sbom::compare(&local.packages, &g.packages);
    crate::views::insights::stats(
        ui,
        &[
            kit::Stat::new("On GitHub", format::count(g.packages.len())),
            kit::Stat::new("Same version", format::count(d.matched))
                .tone_if(d.matched > 0, Tone::Success),
            kit::Stat::new("Only in the lockfiles", format::count(d.only_local.len())),
            kit::Stat::new("Only on GitHub", format::count(d.only_github.len())),
            kit::Stat::new("Version differs", format::count(d.mismatched.len()))
                .tone_if(!d.mismatched.is_empty(), Tone::Warning),
            kit::Stat::new("Ranges on GitHub", format::count(d.ranges))
                .tip("GitHub lists a version range when it only saw a manifest, not a lockfile"),
        ],
    );
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(format!(
                "{}  ·  created {}",
                g.name,
                if g.created.is_empty() {
                    "?"
                } else {
                    g.created.as_str()
                }
            ))
            .size(size::SMALL)
            .color(pal(ui).text_2),
        );
        if IconButton::new(Icon::Download, "Save GitHub's SBOM")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Act::Export("github".into()).into());
        }
        if IconButton::new(Icon::Rerun, "Fetch again")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Act::FetchGhSbom.into());
        }
    });
    if !d.unsupported.is_empty() {
        kit::muted(
            ui,
            &format!(
                "Not compared (ecosystems gitvis does not read locally): {}",
                d.unsupported
                    .iter()
                    .map(|(t, n)| format!("{t} {n}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }
    let table = |ui: &mut Ui, id: &str, title: &str, rows: &[(String, String, Vec<String>)]| {
        if rows.is_empty() {
            return;
        }
        crate::views::insights::group(ui, title, Some(rows.len()));
        let cells: Vec<Vec<Cell>> = rows
            .iter()
            .take(50)
            .map(|(t, n, v)| vec![text(n.as_str()), sec(t.as_str()), mono(v.join(", "))])
            .collect();
        grid(
            ui,
            id,
            &[
                Col::text("Package"),
                Col::text("Type"),
                Col::text("Versions"),
            ],
            &cells,
            false,
            |_, _| {},
        );
        crate::views::insights::more(ui, rows.len(), 50);
    };
    if !d.mismatched.is_empty() {
        crate::views::insights::group(ui, "Version differs", Some(d.mismatched.len()));
        let cells: Vec<Vec<Cell>> = d
            .mismatched
            .iter()
            .take(50)
            .map(|(t, n, l, g)| {
                vec![
                    text(n.as_str()),
                    sec(t.as_str()),
                    mono(l.join(", ")),
                    mono(g.join(", ")),
                ]
            })
            .collect();
        grid(
            ui,
            "gov_sbom_mismatch",
            &[
                Col::text("Package"),
                Col::text("Type"),
                Col::text("Lockfiles"),
                Col::text("GitHub"),
            ],
            &cells,
            false,
            |_, _| {},
        );
        crate::views::insights::more(ui, d.mismatched.len(), 50);
    }
    table(
        ui,
        "gov_sbom_only_local",
        "Only in the lockfiles",
        &d.only_local,
    );
    table(ui, "gov_sbom_only_gh", "Only on GitHub", &d.only_github);
}

// ---------------------------------------------------------------------------
// Container images
// ---------------------------------------------------------------------------

fn worst(findings: &[Finding]) -> Option<Severity> {
    findings.iter().map(|f| f.severity).max()
}

fn finding_badges(findings: &[Finding]) -> Vec<(String, Tone)> {
    let mut by: BTreeMap<Severity, usize> = BTreeMap::new();
    for f in findings {
        if f.severity >= Severity::Medium {
            *by.entry(f.severity).or_insert(0) += 1;
        }
    }
    by.into_iter()
        .rev()
        .map(|(s, n)| {
            let (label, tone) = severity_badge(s);
            (format!("{n} {}", label.to_lowercase()), tone)
        })
        .collect()
}

pub fn images(ui: &mut Ui, repo: &mut RepoState, console: &Console, actions: &mut Vec<Action>) {
    let workdir = repo.workdir.clone();
    let State {
        images,
        image_selected,
        scanners,
        scan_image,
        scan_with,
        scans,
        ..
    } = &mut repo.devops.govern;
    crate::devops::pages::layout::page_header(
        ui,
        "Container images",
        "Dockerfiles and compose files in the working tree, linted locally; image scans on demand",
        |ui| {
            widgets::refresh(
                ui,
                images.loading,
                "Find and lint the Dockerfiles again (local)",
                actions,
            );
            ui.add_space(6.0);
            widgets::fetched(ui, images, "");
        },
    );
    let Some(report) = images.ok() else {
        let error = images.error.clone();
        kit::scroll_page(ui, "gov_img_wait", |ui| match &error {
            Some(e) => widgets::failed(
                ui,
                "Could not list the tracked files",
                e,
                Act::LoadImages,
                actions,
            ),
            None => widgets::loading(ui, "Looking for Dockerfiles and compose files…"),
        });
        return;
    };
    let sel = image_selected.clone();
    crate::devops::pages::layout::split_keyed(
        ui,
        "gov_img",
        sel.as_deref().unwrap_or(""),
        actions,
        |ui, actions| {
            widgets::list_head(
                ui,
                "Dockerfiles",
                report.dockerfiles.len(),
                sel.is_some(),
                Act::SelectDockerfile(None),
                actions,
            );
            if report.dockerfiles.is_empty() {
                widgets::list_none(
                    ui,
                    "No Dockerfile, *.dockerfile or Containerfile is tracked.",
                );
                return;
            }
            kit::list(
                ui,
                "gov_img_list",
                report.dockerfiles.len(),
                kit::ROW_H,
                |i| sel.as_deref() == Some(report.dockerfiles[i].0.path.as_str()),
                |ui, rect, resp, i| {
                    let (df, findings) = &report.dockerfiles[i];
                    let base = df
                        .stages
                        .last()
                        .map(|s| s.image.text.clone())
                        .unwrap_or_default();
                    let sub = format!(
                        "{} · FROM {}",
                        format::plural(df.stages.len(), "stage", "stages"),
                        if base.is_empty() { "?" } else { base.as_str() }
                    );
                    let badges = finding_badges(findings);
                    let dot = match worst(findings) {
                        Some(s) if s >= Severity::Medium => severity_dot(ui, s),
                        _ => pal(ui).success,
                    };
                    kit::paint_row(
                        ui,
                        rect,
                        &Row::new(&df.path)
                            .leading(Leading::Dot(dot))
                            .subtitle(&sub)
                            .badges(&badges),
                        sel.as_deref() == Some(df.path.as_str()),
                    );
                    if resp.clicked() {
                        actions.push(Act::SelectDockerfile(Some(df.path.clone())).into());
                    }
                },
            );
        },
        |ui, actions| match sel
            .as_deref()
            .and_then(|p| report.dockerfiles.iter().find(|(d, _)| d.path == p))
        {
            Some((df, findings)) => dockerfile_detail(ui, &workdir, df, findings, actions),
            None => {
                images_overview(ui, report);
                scan_section(
                    ui, report, scanners, scan_image, *scan_with, scans, console, actions,
                );
                images_tables(ui, report, actions);
            }
        },
    );
}

fn images_overview(ui: &mut Ui, r: &ImagesReport) {
    let stages: usize = r.dockerfiles.iter().map(|(d, _)| d.stages.len()).sum();
    let high = r.finding_count(Severity::High);
    let medium = r.finding_count(Severity::Medium) - high;
    let low = r.finding_count(Severity::Low) - high - medium;
    let images = r.images();
    crate::views::insights::stats(
        ui,
        &[
            kit::Stat::new("Dockerfiles", r.dockerfiles.len().to_string()),
            kit::Stat::new("Stages", stages.to_string()),
            kit::Stat::new("High", high.to_string()).tone_if(high > 0, Tone::Danger),
            kit::Stat::new("Medium", medium.to_string()).tone_if(medium > 0, Tone::Warning),
            kit::Stat::new("Low", low.to_string()),
            kit::Stat::new("Images referenced", images.len().to_string())
                .tip("Distinct base images in FROM lines and image: entries of compose files"),
            kit::Stat::new("Compose services", r.compose.len().to_string()),
        ],
    );
    kit::secondary(
        ui,
        &format!(
            "Linted in {}: tags and digests, the user the container runs as, ADD from URLs, piped install scripts, apt / apk hygiene, secret-looking ENV and ARG names (names only), cache-busting COPY, EXPOSE 22, sudo and HEALTHCHECK. Select a Dockerfile for its findings and fixes.",
            format::millis(r.elapsed_ms)
        ),
    );
    if !r.unreadable.is_empty() {
        kit::callout(
            ui,
            Tone::Warning,
            "Some files were skipped",
            &r.unreadable
                .iter()
                .map(|(p, e)| format!("{p}: {e}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
}

fn images_tables(ui: &mut Ui, r: &ImagesReport, actions: &mut Vec<Action>) {
    let images = r.images();
    if !images.is_empty() {
        kit::section(ui, "Images", Some(images.len()));
        let mut used: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (df, _) in &r.dockerfiles {
            for s in &df.stages {
                used.entry(s.image.text.clone())
                    .or_default()
                    .insert(format!("{}:{}", df.path, s.line));
            }
        }
        for c in &r.compose {
            if let Some(i) = &c.image {
                used.entry(i.text.clone())
                    .or_default()
                    .insert(format!("{} ({})", c.file, c.service));
            }
        }
        let cells: Vec<Vec<Cell>> = images
            .iter()
            .take(80)
            .map(|i| {
                let r = gitvis_core::devops::govern::docker::ImageRef::parse(i);
                let tag = match r.tag.as_deref() {
                    None => Cell::Tone("none".into(), Tone::Warning),
                    Some("latest") => Cell::Tone("latest".into(), Tone::Warning),
                    Some(t) => sec(t),
                };
                vec![
                    mono(r.repo.as_str()),
                    tag,
                    if r.digest.is_some() {
                        Cell::Badges(vec![("Pinned".into(), Tone::Success)])
                    } else {
                        muted("no")
                    },
                    sec(used
                        .get(i)
                        .map(|s| s.iter().cloned().collect::<Vec<_>>().join(", "))
                        .unwrap_or_default()),
                ]
            })
            .collect();
        grid(
            ui,
            "gov_img_images",
            &[
                Col::text("Image"),
                Col::text("Tag"),
                Col::text("Digest"),
                Col::text("Used in"),
            ],
            &cells,
            false,
            |r, resp| {
                if let Some(i) = images.get(r) {
                    resp.context_menu(|ui| {
                        if ui.button("Copy image").clicked() {
                            actions.push(Action::Copy(i.clone()));
                            ui.close();
                        }
                    });
                }
            },
        );
    }
    if !r.compose.is_empty() {
        kit::section(ui, "Compose services", Some(r.compose.len()));
        let cells: Vec<Vec<Cell>> = r
            .compose
            .iter()
            .take(80)
            .map(|c| {
                vec![
                    text(c.service.as_str()),
                    match &c.image {
                        Some(i) => mono(i.text.as_str()),
                        None if c.build.is_some() => muted("built"),
                        None => muted("–"),
                    },
                    match &c.build {
                        Some(b) => sec(b.as_str()),
                        None => muted("–"),
                    },
                    sec(if c.line > 0 {
                        format!("{}:{}", c.file, c.line)
                    } else {
                        c.file.clone()
                    }),
                ]
            })
            .collect();
        grid(
            ui,
            "gov_img_compose",
            &[
                Col::text("Service"),
                Col::text("Image"),
                Col::text("Build"),
                Col::text("File"),
            ],
            &cells,
            false,
            |_, _| {},
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn scan_section(
    ui: &mut Ui,
    report: &ImagesReport,
    scanners: &super::Fetch<Vec<ScannerStatus>>,
    scan_image: &mut String,
    scan_with: Option<Scanner>,
    scans: &[ScanRun],
    console: &Console,
    actions: &mut Vec<Action>,
) {
    kit::section_with(ui, "Scan an image", None, |ui| {
        if !scanners.loading
            && IconButton::new(Icon::Rerun, "Check again")
                .compact()
                .show(ui)
                .on_hover_text("Look for trivy, grype and docker scout again")
                .clicked()
        {
            actions.push(Act::DetectScanners.into());
        }
    });
    kit::secondary(
        ui,
        "A scan pulls the image from its registry (network) and lists its known vulnerabilities. It runs in the console; the report is summarised here.",
    );
    let Some(found) = scanners.ok() else {
        kit::loading(ui, "Looking for trivy, grype and docker scout…");
        return;
    };
    let options: Vec<(Scanner, &str)> = Scanner::ALL.iter().map(|s| (*s, s.label())).collect();
    let current = scan_with.unwrap_or(Scanner::Scout);
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if let Some(s) = kit::segmented(ui, &options, current) {
            actions.push(Act::SetScanner(s).into());
        }
        ui.add_space(8.0);
        for st in found {
            let (t, tone) = match &st.version {
                Some(v) => (format!("{} {v}", st.scanner.label()), Tone::Success),
                None => (format!("{} missing", st.scanner.label()), Tone::Neutral),
            };
            kit::badge(ui, &t, tone);
        }
    });
    let status = found.iter().find(|s| s.scanner == current);
    let usable = status.is_some_and(|s| s.version.is_some());
    match status {
        Some(s) if s.version.is_none() => {
            ui.add_space(4.0);
            match current {
                Scanner::Trivy => widgets::missing_tool(
                    ui,
                    Tool::Trivy,
                    "Trivy scans container images for known vulnerabilities in OS packages and language dependencies, offline after its first database download.",
                    actions,
                ),
                Scanner::Grype => widgets::missing_tool(
                    ui,
                    Tool::Grype,
                    "Grype matches the packages of an image against several vulnerability databases.",
                    actions,
                ),
                Scanner::Scout => {
                    if s.path.is_none() {
                        widgets::missing_tool(
                            ui,
                            Tool::Docker,
                            "Docker Scout ships with Docker Desktop and analyses images against Docker's advisory database.",
                            actions,
                        )
                    } else {
                        kit::callout_with(
                            ui,
                            Tone::Warning,
                            "The docker scout plugin is not available",
                            s.note.as_deref().unwrap_or(""),
                            |ui| {
                                kit::code_block(
                                    ui,
                                    "Update Docker Desktop, or install the CLI plugin: https://github.com/docker/scout-cli",
                                    actions,
                                );
                                if IconButton::new(Icon::Rerun, "Check again")
                                    .compact()
                                    .show(ui)
                                    .clicked()
                                {
                                    actions.push(Act::DetectScanners.into());
                                }
                            },
                        );
                    }
                }
            }
        }
        Some(s) => {
            if current == Scanner::Scout {
                kit::muted(
                    ui,
                    &format!(
                        "docker scout {}: needs a Docker Hub login (docker login) and sends the image's package list to Docker.",
                        s.version.as_deref().unwrap_or("")
                    ),
                );
            } else if let Some(p) = &s.path {
                kit::muted(ui, p);
            }
        }
        None => {}
    }
    ui.add_space(4.0);
    form::field(
        ui,
        "Image",
        Some(
            "A reference such as alpine:3.20 or ghcr.io/acme/app:1.4; the images referenced above are suggested.",
        ),
        |ui| {
            form::input(
                ui,
                "gov_scan_image",
                scan_image,
                form::InputOpts::new("registry/repository:tag").mono(),
            );
        },
    );
    let images = report.images();
    if !images.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
            for i in images.iter().take(12) {
                if kit::chip(ui, i, scan_image == i).clicked() {
                    *scan_image = i.clone();
                }
            }
        });
    }
    let running = scans.iter().any(|s| s.result.is_none());
    form::actions_row(ui, |ui| {
        if form::primary(
            ui,
            &format!("Scan with {}", current.label()),
            usable && !scan_image.trim().is_empty() && !running,
        )
        .on_hover_text("Pulls the image, then scans it (network)")
        .clicked()
        {
            actions.push(Act::Scan(current, scan_image.clone()).into());
        }
    });
    if scans.is_empty() {
        return;
    }
    kit::section(ui, "Scans", Some(scans.len()));
    for run in scans {
        scan_result(ui, run, console, actions);
        ui.add_space(8.0);
    }
}

fn scan_result(ui: &mut Ui, run: &ScanRun, console: &Console, actions: &mut Vec<Action>) {
    let p = *pal(ui);
    let crun = console.get(run.id);
    ui.horizontal(|ui| {
        kit::subtitle(ui, &format!("{} · {}", run.scanner.label(), run.image));
        let (t, tone) = match &run.result {
            None => match crun {
                Some(c) if !c.running() => ("Reading the report".to_owned(), Tone::Accent),
                _ => ("Running".to_owned(), Tone::Accent),
            },
            Some(Ok(_)) => ("Done".to_owned(), Tone::Success),
            Some(Err(ScanFailure::NeedsLogin(_))) => {
                ("Needs docker login".to_owned(), Tone::Warning)
            }
            Some(Err(_)) => ("Failed".to_owned(), Tone::Danger),
        };
        kit::badge(ui, &t, tone);
        ui.label(
            RichText::new(format!(
                "started {} ago",
                format::millis(run.started.elapsed().as_millis() as u64)
            ))
            .size(size::SMALL)
            .color(p.text_2),
        );
    });
    match &run.result {
        None => {
            if let Some(c) = crun {
                ui.label(kit::mono(&c.command).color(p.text_2));
                let w = ui.available_width();
                ui.allocate_ui(egui::vec2(w, 220.0), |ui| {
                    crate::views::console::lines(ui, c, "gov_scan");
                });
            }
        }
        Some(Err(ScanFailure::NeedsLogin(tail))) => {
            kit::callout_with(
                ui,
                Tone::Warning,
                "Docker Scout needs a Docker Hub login",
                "Scout analyses images against Docker's database and only works for a signed-in Docker ID (a free account is enough). Sign in, then scan again.",
                |ui| {
                    kit::code_block(ui, "docker login", actions);
                    kit::code_block(ui, tail, actions);
                },
            );
        }
        Some(Err(ScanFailure::Failed(e))) => {
            kit::callout_with(ui, Tone::Danger, "The scan failed", "", |ui| {
                kit::code_block(ui, e, actions);
            });
        }
        Some(Ok(s)) => {
            crate::views::insights::stats(
                ui,
                &[
                    kit::Stat::new("Critical", s.counts[0].to_string())
                        .tone_if(s.counts[0] > 0, Tone::Danger),
                    kit::Stat::new("High", s.counts[1].to_string())
                        .tone_if(s.counts[1] > 0, Tone::Danger),
                    kit::Stat::new("Medium", s.counts[2].to_string())
                        .tone_if(s.counts[2] > 0, Tone::Warning),
                    kit::Stat::new("Low", s.counts[3].to_string()),
                    kit::Stat::new("Total", s.total.to_string()),
                    kit::Stat::new("Fixable", s.fixable.to_string())
                        .tip("Vulnerabilities with a fixed version available"),
                ],
            );
            if !s.targets.is_empty() {
                kit::muted(
                    ui,
                    &s.targets
                        .iter()
                        .map(|(t, n)| format!("{t}: {n}"))
                        .collect::<Vec<_>>()
                        .join("  ·  "),
                );
            }
            if s.top.is_empty() {
                kit::callout(ui, Tone::Success, "No known vulnerabilities", "");
            } else {
                let cells: Vec<Vec<Cell>> = s
                    .top
                    .iter()
                    .map(|v| {
                        vec![
                            Cell::Badges(vec![severity_badge(v.severity)]),
                            mono(v.id.as_str()),
                            text(v.package.as_str()),
                            mono(v.installed.as_str()),
                            match &v.fixed {
                                Some(f) => Cell::Tone(f.clone(), Tone::Success),
                                None => muted("no fix"),
                            },
                            sec(v.title.as_str()),
                        ]
                    })
                    .collect();
                grid(
                    ui,
                    &format!("gov_scan_{}", run.id),
                    &[
                        Col::text("Severity"),
                        Col::text("Vulnerability"),
                        Col::text("Package"),
                        Col::text("Installed"),
                        Col::text("Fixed in"),
                        Col::text("Title"),
                    ],
                    &cells,
                    true,
                    |r, resp| {
                        if let Some(v) = s.top.get(r)
                            && resp.clicked()
                            && !v.url.is_empty()
                        {
                            actions.push(Action::OpenUrl(v.url.clone()));
                        }
                    },
                );
                crate::views::insights::more(ui, s.total, s.top.len());
            }
        }
    }
}

fn dockerfile_detail(
    ui: &mut Ui,
    workdir: &std::path::Path,
    df: &Dockerfile,
    findings: &[Finding],
    actions: &mut Vec<Action>,
) {
    kit::title(ui, &df.path);
    ui.horizontal_wrapped(|ui| {
        kit::badge(
            ui,
            &format::plural(df.stages.len(), "stage", "stages"),
            Tone::Neutral,
        );
        kit::badge(
            ui,
            &format::plural(df.lines, "line", "lines"),
            Tone::Neutral,
        );
        for b in finding_badges(findings) {
            kit::badge(ui, &b.0, b.1);
        }
    });
    ui.horizontal(|ui| {
        if IconButton::new(Icon::External, "Open file")
            .compact()
            .show(ui)
            .clicked()
        {
            actions.push(Action::OpenPath(workdir.join(&df.path)));
        }
    });
    kit::section(ui, "Stages", Some(df.stages.len()));
    let cells: Vec<Vec<Cell>> = df
        .stages
        .iter()
        .map(|s| {
            vec![
                text(s.label()),
                mono(if s.image.text.is_empty() {
                    "?".to_owned()
                } else {
                    s.image.repo.clone()
                }),
                if s.from_stage || s.scratch {
                    muted("–")
                } else {
                    match s.image.tag.as_deref() {
                        None => Cell::Tone("none".into(), Tone::Warning),
                        Some("latest") => Cell::Tone("latest".into(), Tone::Warning),
                        Some(t) => sec(t),
                    }
                },
                if s.image.digest.is_some() {
                    Cell::Badges(vec![("Pinned".into(), Tone::Success)])
                } else {
                    muted("no")
                },
                match s.final_user() {
                    Some(u) => sec(u),
                    None => muted("root (default)"),
                },
                if s.exposes.is_empty() {
                    muted("–")
                } else {
                    sec(s
                        .exposes
                        .iter()
                        .map(|(_, p)| p.as_str())
                        .collect::<Vec<_>>()
                        .join(" "))
                },
                num(s.runs),
                sec(format!("{}", s.line)),
            ]
        })
        .collect();
    grid(
        ui,
        "gov_df_stages",
        &[
            Col::text("Stage"),
            Col::text("Base image"),
            Col::text("Tag"),
            Col::text("Digest"),
            Col::text("User"),
            Col::text("Exposes"),
            Col::num("RUN"),
            Col::num("Line"),
        ],
        &cells,
        false,
        |_, _| {},
    );
    if !df.global_args.is_empty() {
        kit::muted(
            ui,
            &format!("Build arguments before FROM: {}", df.global_args.join(", ")),
        );
    }
    kit::section(ui, "Findings", Some(findings.len()));
    if findings.is_empty() {
        kit::callout(
            ui,
            Tone::Success,
            "No findings",
            "Pinned base images, a non-root user, a health check, and no risky instructions.",
        );
        return;
    }
    // One card per rule; repeats (one per stage) list their lines.
    let mut rules: Vec<(&str, &str)> = Vec::new();
    for f in findings {
        if !rules.contains(&(f.rule, f.explanation)) {
            rules.push((f.rule, f.explanation));
        }
    }
    for (rule, why) in rules {
        let same: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.rule == rule && f.explanation == why)
            .collect();
        let f = same[0];
        ui.add_space(4.0);
        form::card(ui, widgets::severity_tone(f.severity), |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (t, tone) = severity_badge(f.severity);
                kit::badge(ui, &t, tone);
                kit::subtitle(ui, &f.title);
                let lines: Vec<String> = same.iter().map(|x| x.line.to_string()).collect();
                ui.label(
                    RichText::new(format!(
                        "{} {}  ·  {}",
                        if same.len() == 1 { "line" } else { "lines" },
                        lines.join(", "),
                        f.rule
                    ))
                    .size(size::SMALL)
                    .color(pal(ui).text_2),
                );
            });
            if same.len() > 1 {
                for x in &same[1..] {
                    kit::muted(ui, &format!("Also: {} (line {})", x.title, x.line));
                }
            }
            ui.add_space(2.0);
            kit::body(ui, f.explanation);
            ui.add_space(2.0);
            ui.label(RichText::new("Fix").size(size::SMALL).color(pal(ui).text_2));
            let mut fixes: Vec<&str> = Vec::new();
            for x in &same {
                if !fixes.contains(&x.fix.as_str()) {
                    fixes.push(&x.fix);
                }
            }
            kit::code_block(ui, &fixes.join("\n"), actions);
        });
    }
}
