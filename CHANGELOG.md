# Changelog

All notable changes to Hadion are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/saziknows/hadion-gui/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/saziknows/hadion-gui/releases/tag/v0.1.0
