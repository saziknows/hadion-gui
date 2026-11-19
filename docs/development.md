# Developing Hadion

How to build, test and package Hadion, and how the code is laid out. Contributions are
welcome; see [CONTRIBUTING.md](../CONTRIBUTING.md).

## Build and test

Requirements: a current stable Rust toolchain (edition 2024; developed with 1.97), a C
compiler for libgit2 (MSVC or MinGW on Windows, gcc or clang elsewhere), and `git` on `PATH`
at runtime. On Linux the keychain support also needs `pkg-config` and `libdbus-1-dev`.

```sh
cargo run --release -p hadion -- path/to/repo
```

Run without arguments inside a repository to open it, or open one from the
welcome screen (`Ctrl+O`).

Tests:

```sh
cargo test
```

Packaging (Windows): `./scripts/package.ps1` builds the release exe, a zip, `SHA256SUMS` and,
when Inno Setup is installed (`winget install JRSoftware.InnoSetup`), a per-user installer
(`packaging/windows/hadion.iss`, no admin rights). Pushing a tag `vX.Y.Z` that matches
`Cargo.toml` runs `.github/workflows/release.yml`, which builds and uploads the same files;
`.github/workflows/ci.yml` checks format, clippy and tests on pushes and pull requests.
In-app updates need a per-user install (`%LOCALAPPDATA%\Programs\hadion`).

The integration tests build throwaway repositories with the real `git` binary.
`HADION_TIMING=1` prints loader phase timings to stderr;
`cargo run --release -p hadion-core --example bench -- path/to/repo` times a load;
`--example scan -- path/to/repo [--history N]` prints the exposure report,
`--example osint -- path/to/repo [--unreachable]` the OSINT report and
`--example insights -- path/to/repo [--commits N] [--weight] [--lost] [--date-debt] [--pickaxe TEXT]`
the insights report.

Third-party licences: `cargo run -p hadion-core --example notices` writes the licence texts of
every crate compiled into Hadion; `scripts/package.ps1` puts them in
`licenses/THIRD-PARTY-LICENSES.txt` of every release.

README screenshots: `./scripts/readme-shots.ps1` (Windows) builds demo repositories with a
made-up identity and drives the debug build through `HADION_SCRIPT`, one scene per picture,
each from an empty `HADION_STATE_DIR` so your own settings and panel sizes never show. Add
`-GitHub` for the Review and Actions pictures, which read this repository through the
`gh_guard` example (writes blocked). Look at every picture before committing it.


## Architecture

```
crates/
  hadion-core/   repository model, graph layout, diff parsing, git operations
    src/graph.rs     lane layout (pure, unit-tested)
    src/repo.rs      libgit2 snapshot loader; `git status` for the working tree
    src/ops.rs       GitOp -> git command lines, run via the git binary
    src/diff.rs      unified-diff parser: raw bytes for `git apply`, line pairing, word diff
    src/reflog.rs    HEAD reflog and the undo derived from it
    src/github.rs    pull requests, runs, jobs, logs, checks via `gh api` (rate limit tracked)
    src/joblog.rs    Actions job-log parser: timestamps, ##[group]/##[error] markers, ANSI
    src/secrets.rs   secret and sensitive-file rules; working-tree, staged and history scans
    src/osint.rs     identities and time patterns, footprint rules, file metadata, history leftovers, supply chain
    src/insights/    health, numstat history analytics, refs, debt, CI, licences, search, recovery
    src/devops/      no-UI DevOps areas: gh.rs (typed gh api, bodies on stdin), proc.rs
                     (streamed processes), tools.rs, review/, ci/, ship/, runtime/, govern/,
                     workbench/, forge/ (GitLab, Azure DevOps), home/, conflict/,
                     history/, gitplus/, ai/, palette/, identity/, terminal/,
                     automation/, packaging/; safety.rs (safe mode), keychain.rs
    tests/           end-to-end tests against generated repositories
  hadion/        egui application
    src/app.rs       state machine: loads, operations, watcher, actions, verbose logging
    src/worker.rs    background reader thread + one thread per operation or API request
    src/watcher.rs   file-system watcher with .git-aware classification
    src/highlight.rs syntect colouring, computed on the reader thread
    src/script.rs    HADION_SCRIPT smoke-test driver (screenshots via egui)
    src/theme.rs     design tokens: palette, type scale, spacing, fonts
    src/views/kit.rs the component kit every page is built from
    src/views/       rail, toolbar, sidebar, graph, details, diff, github, security, osint, insights/,
                     form (inputs), console (process output), activity, …
    src/devops/      one module per DevOps area (state, actions, replies, views), the Ctx
                     command buffer they use, the console, the Ship page navigation,
                     app-wide areas (global.rs), hooks into shared views (hooks.rs),
                     the Settings page
```

The UI never blocks on git: snapshots, details and diffs are loaded on a
reader thread, operations run on their own thread, and results come back as
events. Views emit `Action`s which the app applies after each frame, so views
stay free of orchestration logic. DevOps areas never touch the app directly: their
controllers push effects (GitHub or local reads, changes, console processes, actions) into
a `Ctx`, and their replies come back as messages routed to the same area.
