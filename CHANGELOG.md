# Changelog

All notable changes to Hadion are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Navigation rail**: page names now sit beside the icons, with more padding, and the rail
  collapses to icons (`Ctrl+B`, the Collapse button, or Settings › Appearance, which also
  offers an automatic mode for narrow windows). Hover is a soft highlight over the whole
  row instead of a block behind the icon, the current page has an accent bar, and
  explanations open beside the rail once the pointer rests.
- **README**: a short tour with a screenshot of each main page; the full feature list moved to
  `docs/features.md`, building and architecture to `docs/development.md`.
- The Containers tab's context bar splits into two rows sooner, so its checkbox no longer
  runs off the edge at 1,440 points.

### Added

- **Ship › Containers**: the compose projects and containers of the open repository on any
  docker context (never switched), live through `docker events` while the tab is shown
  (polling every 10 s if the stream ends): state, health with the last probes, exit codes
  explained, ports, mounts, environment variable names only, live stats, and which commit
  each container runs. Logs in the console, a shell in a terminal tab, start / stop /
  restart / pause, compose up / down / pull / build, and builds labelled with the commit,
  remote and time (optionally scanned with Trivy or Grype). Kill, remove, compose down and
  every change on a remote Docker ask first with the exact command.
- Safe mode also refuses docker commands that change containers, images or compose
  projects on a remote endpoint (`ssh://`, `tcp://`, `http(s)://`).
- The terminal can open a tab running a program instead of a shell (`term-exec` in
  scripts).
- `fake_docker` example: a docker test double for `HADION_DOCKER`.
- `rail` and `pointer X Y` script commands (`pointer` hovers without moving the mouse),
  `HADION_STATE_DIR` (start from default settings and panel sizes) and
  `scripts/readme-shots.ps1`, which retakes the README screenshots.

### Fixed

- The tests that run the `fake_claude` and `fake_docker` examples find them under Cargo
  1.100's new build folder layout.

## [0.1.1] - 2026-10-30

The first release with downloads; 0.1.0 below lists what Hadion does.

### Fixed

- Release packaging: 0.1.0 was published without its zip, installer and checksums, because
  GitHub's immutable releases refuse files added after publishing. Releases are now created as
  drafts, filled, then published.

## [0.1.0] - 2026-10-29

The first public release.

### Added

- **History**: commit graph with colour-stable lanes for 100k-commit repositories, branches,
  tags, stashes, drag-and-drop merge and rebase, per-hunk and per-line staging, side-by-side
  diffs with word emphasis, reflog-based undo, multi-select with cherry-pick, patches and
  squash, graph filters, signature verification, and a merge conflict editor.
- **Compare**: any two refs or the working tree, files at a commit, and blame.
- **Review** and **Actions**: pull requests with inline review, issues, workflow runs with full
  logs, re-runs, dispatch, artifacts, caches, workflow validation and local runs with `act`;
  GitLab and Azure DevOps through `glab` and `az`.
- **Ship**: releases, environments and deployments, where a commit has reached, DORA metrics,
  GitHub Packages, Kubernetes and Terraform, and a release commit helper.
- **Exposure**: secrets and sensitive files in the working tree, staged changes and history,
  plus what the repository reveals to outsiders.
- **Insights**: repository health, hotspots, owners, branches, commit hygiene, releases,
  timeline, technical debt, CI, licences, search and recovery of lost work.
- **Govern**: Dependabot and code-scanning alerts, SBOM export, Dockerfile checks, branch
  protection, CODEOWNERS, environments, secrets and variables.
- **Tools**: tasks, a process console, interactive rebase, bisect, worktrees, file history,
  stashes, submodules, Git LFS, patches and hooks (including a pre-commit secret check).
- **App**: command palette, terminal panel, AI assist through your own Claude Code CLI (your
  Claude plan) or the Claude API, settings for git config, identities, keys and signing, safe
  mode, desktop notifications, repository groups, issue-tracker links, in-app updates and crash
  reports. Icons from the Lucide set; every page fits windows down to 900 × 560.

[Unreleased]: https://github.com/saziknows/hadion-gui/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/saziknows/hadion-gui/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/saziknows/hadion-gui/releases/tag/v0.1.0
