# Contributing to gitvis

Thanks for helping. Bug reports, ideas and pull requests are all welcome. For anything larger
than a fix, please open an issue first so we can agree on the approach before you spend time
on it.

## Build, test, check

```sh
cargo run -p gitvis -- path/to/repo   # debug build; dependencies are optimised, so it stays usable
cargo test                            # unit tests plus integration tests that build throwaway repos with real git
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
```

CI runs format, clippy (warnings are errors) and the tests on Windows and Linux for every push
and pull request. On Linux, install `pkg-config` and `libdbus-1-dev` first. The integration
tests need `git` on `PATH` and a git identity (`user.name`, `user.email`).

## Where things are

- `crates/gitvis-core`: everything without UI (repository snapshots, graph layout, diffs,
  git operations, the exposure scanner, insights, the DevOps areas' requests and parsers).
- `crates/gitvis`: the egui app. Views only read state and push `Action`s; `app.rs` applies them.
- [README.md](README.md#architecture) has the overview. [CLAUDE.md](CLAUDE.md) has detailed
  notes on every module, the DevOps area contract, egui pitfalls we have hit, and the scripted
  smoke tests; it is written for AI coding assistants but reads fine for people too.

## Ground rules

gitvis handles credentials and can change remote systems, so a few rules are not negotiable:

- **Secrets** (tokens, API keys, secret values) never appear on a command line, in the activity
  log, a toast or an error. They go to child processes on standard input and are kept in the OS
  keychain, never in settings files.
- **Network** calls happen only on an explicit action, or when the user opens the page that
  needs them. Background polling only when the user switched it on.
- **Changes outside the machine** (GitHub, GitLab, Azure DevOps, registries, clusters) are built
  by pure request builders with unit tests, and the user sees the exact commands or request
  bodies in a confirmation before anything runs. They must respect safe mode
  (`gitvis_core::devops::safety`).
- **Never test a write against a real remote.** Point `GITVIS_GH` at the `gh_guard` example
  (`cargo build -p gitvis-core --example gh_guard`): it passes reads to the real `gh` and blocks
  every write. Test write flows up to their confirmation dialog.
- **UI** is built from the component kit (`crates/gitvis/src/views/kit.rs`) and the theme tokens
  (`theme.rs`): no hard-coded colours or font sizes, sentence-case labels, nothing under 11 pt.
  Check both themes.

## UI changes

Please include screenshots. The app can drive itself and save screenshots rendered by egui, so
nothing on your desktop gets in the way:

```sh
GITVIS_NO_RESTORE=1 GITVIS_SCRIPT="wait 8; head; shot before.png; theme; shot light.png; quit" target/debug/gitvis.exe path/to/repo
```

Scripted runs never save settings; `GITVIS_NO_RESTORE=1` keeps your saved tabs out of the window.

`scripts/make-demo-repo.sh DIR` builds a demo repository with branches, merges, tags, a remote,
a stash and a dirty tree; `scripts/make-conflict-repos.sh DIR` builds repositories stopped in
merge, diff3, zdiff3 and rebase conflicts; `scripts/make-leaky-repo.sh DIR` plants fake secrets
for the Exposure page. Script commands are listed in CLAUDE.md and `crates/gitvis/src/script.rs`.

## Pull requests

- Keep each pull request to one topic, with a description of what changes and why.
- Add tests for parsers, request builders and anything with logic; integration tests that
  need a repository build one with the real `git` binary (see `crates/gitvis-core/tests/`).
- Make sure `cargo fmt`, `cargo clippy -- -D warnings` and `cargo test` pass.

## License

By contributing, you agree that your contributions are dual licensed under the
[MIT license](LICENSE-MIT) and the [Apache License, Version 2.0](LICENSE-APACHE), like the
rest of gitvis, without any additional terms or conditions.
