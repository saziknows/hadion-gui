# gitvis — notes for Claude Code

Native Git client (Rust + egui 0.37) focused on branch visualisation; see README.md for features.

## Commands

- `cargo build` / `cargo run -p gitvis -- <repo>` — debug build; deps are built with opt-level 2 so it stays usable.
- `cargo test` — unit tests plus `crates/gitvis-core/tests/fixture_repo.rs`, which builds throwaway repos with the real `git` binary.
- `cargo run --release -p gitvis-core --example bench -- <repo>` — loader timings (`GITVIS_TIMING=1` prints phases).
- `cargo build --release` — the shippable binary at `target/release/gitvis.exe`.

Toolchain on this machine: `stable-x86_64-pc-windows-gnu` with WinLibs gcc; libgit2 is vendored and compiles from C.

## Layout

- `crates/gitvis-core` — no UI. `graph.rs` (lane layout, pure), `repo.rs` (libgit2 snapshot; working tree via
  `git status --porcelain=v2` because libgit2 status is ~15x slower on big trees), `ops.rs` (`GitOp` → git
  command lines; mutations always go through the git binary; `gh run rerun/cancel` and `gh pr checkout`
  are `GitOp`s too, with `program() == "gh"`), `diff.rs` (unified diff parser keeping raw bytes so hunks
  round-trip through `git apply`), `github.rs` (every `gh api` call is recorded as an `ApiCall` with bytes,
  latency and the `X-Ratelimit-*` headers read from `gh api -i`), `joblog.rs` (Actions log parser),
  `secrets.rs` (exposure scanner: `SPECS` content rules with placeholder/entropy filters, gated by
  per-rule keywords through one aho-corasick pass per line (`keywords_for`; without the gate the
  regexes manage ~1 MB/s per thread), `FILE_RULES` for sensitive names, `scan()` = files on disk in
  parallel + streamed `git log -p -U0 --diff-filter=AM` history + HEAD check (`git diff --name-only
  HEAD`: unchanged tracked files equal HEAD, changed ones are read via `git cat-file --batch`; never
  `git grep` with hundreds of patterns, it is O(patterns × lines)) + `git rev-list --remotes` for
  "pushed"; `scan_changes()` is the quick staged/unstaged/untracked scan that runs after every status
  refresh). Secrets must never reach a command line or the activity log; findings are keyed by
  `fingerprint(rule, secret)` and dismissals live in `RepoPrefs.dismissed`.
  `cargo run --release -p gitvis-core --example scan -- <repo> [--history N]` prints a report.
  `osint.rs` reuses the scanner (`Rules::from_specs` with `strict: false` + validators for the footprint
  rules; `Collector`/`scan_files`/`walk_patch` are `pub(crate)`) and adds: one `git log --format=%x1e…%B`
  pass for identities, time heat maps and message intelligence; EXIF/PNG/PDF/Office parsers (zip via
  `flate2`); `rev-list --reflog --not --all` (rewritten commits), `stash show -p --include-untracked`,
  `for-each-ref` (backup names), `log --diff-filter=D --name-only` (deleted sensitive files); lockfile
  parsers; on demand `fsck --unreachable --connectivity-only` + `cat-file --batch` blob scan,
  `ls-remote refs/pull/*/head`, osv.dev through `curl`, GitHub forks/alerts/admin/code-search in
  `github.rs`. `--example osint -- <repo> [--unreachable]` prints it.
  `insights/` (Insights page): `mod.rs` holds `InsightsOptions`/`InsightsReport`, `tracked_files`
  (`ls-files -s` + `ls-tree -r -l HEAD` sizes), `stream_log` (streamed `git log --numstat -M` with a
  `%x1e`/`%x1f` record format; the one expensive pass, capped by `max_commits`, default 10,000) and
  `analyze` which times each phase into `stats.phases`. `health.rs` (landmines from the index listing,
  `ls-files --eol`, exec bits, `count-objects`, on-demand `weight` = `rev-list --objects --all` piped
  into `cat-file --batch-check=… %(rest)` so no oid→path map is needed), `history.rs` (hotspots,
  coupling, age, ownership, quality, timeline from the log pass; `blame` of one file), `refs.rs`
  (`for-each-ref`, `rev-list --count`, `merge-base` + `diff --name-only` for conflict prediction,
  `cherry` for duplicates; releases from tags), `debt.rs` (parallel marker scan, `date` = parallel
  `git blame --porcelain`), `ci.rs` (serde_yaml over `.github/workflows`), `license.rs`,
  `search.rs` (`log -S/-G/--grep/--author --name-only`, streamed, capped), `recover.rs`
  (`fsck --unreachable --no-reflogs --connectivity-only` + `log --no-walk --stdin` + `reflog --all`).
  `--example insights -- <repo> [--weight] [--lost] [--date-debt] [--pickaxe TEXT]` prints it.
- `crates/gitvis` — egui app. `app.rs` owns all orchestration (loads, ops, watcher debounce, selection,
  the verbose activity log); views in `src/views/` only read state and push `Action`s. Background work:
  `worker.rs` (one reader thread that coalesces stale jobs, one thread per op / API request),
  `watcher.rs` (notify; `.git/objects`, `index.lock` ignored). `views/github.rs` is the Actions page's
  Runs tab (runs → jobs → steps → log viewer; `checks_section` is shared with the details panel); its
  other tabs come from `devops::ci`, and pull requests live on the Review page (`devops::review`).
  `views/security.rs` is the Exposure page (`visible_items` applies the filters; the commit guard in
  `views/details.rs` reads `sec_page.report`, which is the full scan merged with the quick scan);
  its OSINT tabs live in `views/osint.rs` (`SecurityTab`, `OsintState` in `state.rs`; the analysis
  starts on the first visit of an OSINT tab, GitHub data on the GitHub tab). `views/insights/` is the
  Insights page (`InsightsTab`, `InsightsPage` in `state.rs`; one file per tab group; `mod.rs` has the
  shared `list`/`dot_row`/`tiles`/`hbar`/`bars` widgets and re-exports the osint `split`/`column`
  helpers). The analysis starts on the first visit; weight, debt dating, blame, search and recovery run
  on demand (`GitOp::StashStore` restores a dropped stash).
- Design system (2026-10-26 redesign; the owner found the old UI hard to digest): `theme.rs` holds the
  tokens (`pal(ui)` palette with surfaces chrome/panel/canvas/raised/hover/overlay, text/text_2/text_3/
  text_strong, status colours; `Tone`; `size::` type scale with nothing under 11 pt; `space::`, `radius::`)
  and loads Inter (one variable font pinned at 400/500/600 via `FontTweak.coords`) and JetBrains Mono from
  `crates/gitvis/assets/fonts` (OFL licences beside them). egui's `.strong()` only brightens colour;
  `.family(theme::semibold())` / `theme::medium()` set real weights. `views/kit.rs` is the component kit every
  page builds from (text, sections, cards, callouts, code blocks, key-value, stats, badges, bars, tables,
  virtualised two-line rows, search box, chips, segmented control, section nav, page frames, progress).
  Shell: `views/rail.rs` (left navigation: Home, History, Review, Actions, Ship, Exposure, Insights,
  Govern, Tools; Activity and Settings at the bottom; a dot only when something needs attention, which
  each DevOps area reports through its `rail_badge`), `views/toolbar.rs` (repo tabs + git actions
  only), `views/statusbar.rs` (location, working tree, last activity). The refs sidebar shows on the
  History page only; Exposure and Insights use `kit::page_with_nav` (grouped section navigation).
- GitHub specifics: job logs need `gh api --allow-escape-sequences` (gh refuses ANSI output otherwise)
  and are unavailable for running or never-started jobs (HTTP 404 from blob storage). An Actions job's id
  is its check-run id (annotations live there). Check runs of a commit are fetched 400 ms after it is
  selected, so arrow-keying through the graph does not spend the 5,000/h API budget.

## DevOps areas (`crates/gitvis-core/src/devops/`, `crates/gitvis/src/devops/`)

Everything around the repository: review loop, CI control, releases and deployments, what is running,
governance, the local workbench, GitLab / Azure DevOps, the cross-repository home page. One area =
one core module (no UI) + one app module; `app.rs` only routes (`Action::DevOps`, `Event::DevOps`,
`Event::Console*`) and never needs an area's internals.

| Area (app `devops/X`) | Page | Core `devops::X` |
|---|---|---|
| `home` (global state in `GitVis.home`) | Home (`Page::Home`) | inbox, dashboard rows |
| `review` | Review (`Page::Review`) | PRs, reviews, threads, issues |
| `ci` | Actions tabs beside Runs (`ci::Tab`; Runs is `views/github.rs`) | dispatch, artifacts, caches, history, compare, lint, act |
| `ship`, `runtime` | Ship (`devops/pages.rs` owns the nav, `ShipTab`) | releases, deployments, DORA / GHCR, kubectl, terraform |
| `govern` | Govern (`Page::Govern`) | alerts, SBOM, images, protection, CODEOWNERS, envs, secrets |
| `workbench` | Tools (`Page::Workbench`) | tasks, interactive rebase, bisect, worktrees, file history |
| `forge` | Review / Actions when `RepoState::forge_remote` is GitLab / Azure | `glab api`, `az` |

- App module contract: `State` (in `RepoState::devops`), `Act` (Clone + Debug; `impl From<Act> for
  Action` exists, so views do `actions.push(Act::X.into())`), `Msg` with `summary()` for the log,
  `Prefs` (serde, persisted in `RepoPrefs::devops`), `apply`, `on_msg`, `on_console_done`,
  `on_page_shown` (Ship areas: `on_tab_shown`), `script(cmd, arg)`, `rail_badge`, `show` / `show_tab`.
- `devops::Ctx` is a command buffer: `gh_read` / `user_read` (GitHub API, logged as API activity),
  `read` (local, logged as a read with the commands pushed to `TaskLog`), `gh_change` / `remote_change`
  (always logged as an operation with every request and its JSON body, toast), `local_change` (busy
  indicator, operation log, repository reload), `console(title, ProcSpec)` (streamed process; returns
  the run id; every area gets `on_console_done`), `action`, `toast`, `ops` (GitOps through the normal
  pipeline), `confirm` (`Dialog::ConfirmAction` showing the exact commands / bodies, then an action).
- Core helpers: `devops::gh::Gh` (`path`, `get_json`, `get_pages`, `get_wrapped_pages`, `send` /
  `send_json` of a `Request` built by a pure function, `graphql`, `cli` for other `gh` subcommands with
  never-logged stdin, `encode`); `github::api_request` underneath records `ApiCall { input }` (the JSON
  body, or only its size for `Request::withheld()`), and error messages carry GitHub's `message`, every
  `errors[]` entry and `(HTTP nnn)`. `devops::proc` (`ProcSpec` with env / stdin / `with_color`,
  `resolve` through PATHEXT so `npm` finds `npm.cmd`, `run_streaming`, `run_capture` with timeout,
  `kill_tree`). `devops::tools` (`Tool`, `available`, `detect` (cached version), `install_hint`).
  `GITVIS_GH` overrides the `gh` executable (a recording test double).
- Views: `views/form.rs` (inputs with stable ids, `field`, `row`, `checkbox`, `radio`, `select`, `tags`,
  `primary` / `button` / `danger`, `actions_row`, `card`, `error`), `views/console.rs` (`output`,
  `lines`, `run_list`; `ConsoleAction` stop / rerun / copy / remove).
- Rules: secret values only on standard input (`gh secret set NAME` reads stdin; `Request::withheld`),
  never in argv, logs, toasts or errors; masked inputs cleared after sending. Network calls only on an
  explicit action or when the user opens the page / tab that needs them. Writes are pure request
  builders with unit tests. Never run a write against a real remote, registry or cluster while
  testing (brave-core, the owner's GitHub repositories, the local kube contexts are read-only test
  beds); test writes through their request builders and stop UI flows at the confirmation dialog.
  `kubectl` always gets an explicit `--context`; never change the user's current context or kubeconfig.
- Script commands: anything unknown goes to `devops::script` → each area's `script`. Built in: `page
  home|graph|review|actions|ship|exposure|insights|govern|tools`, `ship TAB`, `ship-tab TAB`.

## egui 0.37 gotchas (verified against the crate source)

- `eframe::App::ui(&mut self, ui, frame)` replaces `update`; panels are `egui::Panel::left(..).show(ui, ..)`.
- A side panel shrinks to its content's width: call `ui.set_min_width(ui.available_width())` inside it.
- Widgets whose position in the layout can change between frames need an explicit `id_salt`, or they lose
  focus (the search box was dropping keystrokes because of this).
- `Id::new` is deprecated in favour of `Id::unique`; `Frame::NONE` is the empty frame.
- Never put an empty `scope_builder` child inside a virtualised row: it hands egui a degenerate
  rect and corrupts the row layout. For overlaid buttons use `ui.new_child(...)` (detached).
- `horizontal()` inherits a parent's right-to-left layout; use `with_layout(left_to_right)`
  explicitly inside right-aligned toolbars.
- GitHub data comes from `gh api` (the user's `gh auth`); syntect's bundled grammars lack
  TypeScript/Swift/Kotlin, so `highlight.rs` falls back to JS/C/Java and hides the language label.
- Never put `Label::truncate()` cells in an `egui::Grid`: a column is as wide as its widest cell and a
  truncating label is happy with any width, so the column collapses ("2026-1…"). Use
  `wrap_mode(TextWrapMode::Extend)` for grid values.
- Nested `Panel::left/bottom(..).show(ui, ..)` inside a `CentralPanel` works, but the remaining area
  must then be claimed with another `CentralPanel::default().show(ui, ..)` (see `views/github.rs`).
- Never use `ui.put` / `scope_builder` to draw a button over a row, tab or block: they *set* the
  parent's layout cursor to the end of the placed rect (egui `advance_after_rects`), so content after
  it shifts while the button is shown (repo tabs jumped left on hover). Use `kit::overlay`, which places
  the widget in a detached `new_child`. Also derive hover of a container from
  `ui.rect_contains_pointer(rect)`, not `resp.hovered()`, when a child widget sits on top of it.
- A bottom panel shrinks to its content's height on the first frame and egui stores that size: call
  `ui.set_min_height(ui.available_height())` inside it (the GitHub log viewer was stuck at ~140 px).
- `with_layout(left_to_right(Center))` inside a tall top-down pane centres vertically and fills the
  pane; give such rows an exact size with `allocate_ui_with_layout(vec2(w, h), ..)` (the kit's
  search box and segmented control do).
- Tooltips: `on_hover_text("")` draws an empty box (guard empty tips); `on_hover_text` anchors below the
  widget's left edge, so on a pane-wide widget (bars, wrapped lines) it lands far from the cursor: use
  `on_hover_text_at_pointer` there. Do not put a tooltip on every list item or path label: moving across
  a dense page then pops tooltips continuously, which the user reads as "glitching". `tooltip_grace_time`
  is 0 in `theme.rs` for the same reason.
- Readability rules learnt the hard way (`views/insights/mod.rs` has the helpers): explanations at 12.5 px
  or more (never `.small()` for prose), prose capped at `PROSE_WIDTH`, section headers with a hairline,
  `why_box` with a caption, 20 px tile values. `.small()` is for badges only.

## Scripted smoke tests (preferred over mouse automation)

`GITVIS_SCRIPT="wait 10; tab 1; head; open path/in/commit; wait 2; shot out.png; split; shot out2.png; quit"`
drives the app from inside and saves PNGs rendered by egui (works whatever is on screen).
Commands: `wait S`, `shot PATH`, `tab N`, `head`, `wip`, `open PATH`, `open-wip PATH`, `split`,
`unified`, `select HUNK FROM TO`, `activity`, `reflog`, `collapse KEY`, `stage PATH`, `undo`,
(scripted runs never save settings, window geometry or egui memory, so they cannot change the owner's
session; run a copy of the exe under another name, e.g. `gitvis-test.exe`, so the build is never locked),
`theme`, `github` / `graph` / `prs` (centre page), `filter TEXT` and `status failed|running|passed|all`
(run list), `run N` (open the N-th listed run), `job N` (show the N-th job's log), `pr N`, `verbose`,
`security` / `scan` / `scan-history N` / `finding N` / `reveal` (Exposure page), `sec-tab NAME` /
`osint` / `item N` / `unreachable` / `osv` / `dorks` / `pr-refs` (OSINT tabs), `insights` /
`ins-tab NAME` / `analyse N` / `ins-item N` / `weight` / `date-debt` / `pickaxe TEXT` / `grep TEXT` /
`recover` / `blame PATH` (Insights page), `quit`. See
`crates/gitvis/src/script.rs`. `stage`/`undo` run real git operations; the GitHub commands make
real `gh api` calls (brave-core is the live test bed: `status failed; run 0` lands on a Dependabot
failure with annotations and an `##[error]` line). `scripts/make-leaky-repo.sh DIR` builds a
repository with planted (fake) secrets in history, HEAD, the index and the working tree for the
Exposure page, plus OSINT material (second identity, footprint identifiers, stash with a secret,
backup branch, amended commit, deleted key file, junk, git-pinned dependency, CODEOWNERS);
`security; scan-history 0; finding 0` shows the first finding's detail, `sec-tab history` the
leftovers. The same script also plants Insights material (reserved/colliding names via
`update-index --cacheinfo`, CRLF, a script without +x, tags + unreleased commits, a conflicting
branch, a cherry-picked duplicate, a `tmp-old` branch, TODO/FIXME, a leaky workflow, MIT LICENSE vs
`"license": "ISC"`, a foreign copyright header, a dropped stash): `insights; wait 4; shot x.png`
then `ins-tab ci; ins-item 0`. The user usually has their own gitvis instance open on the same
desktop and may click in the scripted test window, so a screenshot can show a different tab than
the script set.

## UI testing without a human

`scripts/make-demo-repo.sh <dir>` builds a demo repository (branches, merges, tags, remote, stash, dirty tree).
`scripts/ui-shot.ps1 -Out x.png` screenshots the gitvis window; `scripts/ui-input.ps1 -Action click|rclick|
dblclick|drag|scroll|keys -X .. -Y ..` sends input in window-relative coordinates (same space as the
screenshots). Launch `target/debug/gitvis.exe <repo>` in the background first; the exe is locked while it runs,
so stop it before rebuilding.
