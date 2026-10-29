# Hadion — notes for Claude Code

Native Git client (Rust + egui 0.37) focused on branch visualisation; see README.md for features.

## Commands

- `cargo build` / `cargo run -p hadion -- <repo>` — debug build; deps are built with opt-level 2 so it stays usable.
  Never share one `CARGO_TARGET_DIR` between worktrees of this repository: cargo names the workspace crates'
  artifacts the same in every worktree and then treats another worktree's build as up to date, so an exe can
  silently contain another branch's code (check with `grep -a -o 'hadion-wt[\/][a-z0-9-]*' exe | sort -u`).
- `cargo test` — unit tests plus `crates/hadion-core/tests/fixture_repo.rs`, which builds throwaway repos with the real `git` binary.
- `cargo run --release -p hadion-core --example bench -- <repo>` — loader timings (`HADION_TIMING=1` prints phases).
- `cargo build --release` — the shippable binary at `target/release/hadion.exe`.
- `cargo run -p hadion-core --example notices -- --out FILE` — third-party licence notices (every crate compiled in, with
  its licence files; `scripts/package.ps1` ships them as `licenses/THIRD-PARTY-LICENSES.txt`).

Developed on Windows with the `stable-x86_64-pc-windows-gnu` toolchain and WinLibs gcc (CI also builds with MSVC and on
Linux); libgit2 is vendored and compiles from C.

## Layout

- `crates/hadion-core` — no UI. `graph.rs` (lane layout, pure), `repo.rs` (libgit2 snapshot; working tree via
  `git status --porcelain=v2` because libgit2 status is ~15x slower on big trees), `ops.rs` (`GitOp` → git
  command lines; mutations always go through the git binary; `gh pr checkout`
  is a `GitOp` too, with `program() == "gh"`), `diff.rs` (unified diff parser keeping raw bytes so hunks
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
  `cargo run --release -p hadion-core --example scan -- <repo> [--history N]` prints a report.
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
- `crates/hadion` — egui app. `app.rs` owns all orchestration (loads, ops, watcher debounce, selection,
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
- Design system (2026-10-26 redesign, because the old UI was hard to digest): `theme.rs` holds the
  tokens (`pal(ui)` palette with surfaces chrome/panel/canvas/raised/hover/overlay, text/text_2/text_3/
  text_strong, status colours; `Tone`; `size::` type scale with nothing under 11 pt; `space::`, `radius::`)
  and loads Inter (one variable font pinned at 400/500/600 via `FontTweak.coords`) and JetBrains Mono from
  `crates/hadion/assets/fonts` (OFL licences beside them). egui's `.strong()` only brightens colour;
  `.family(theme::semibold())` / `theme::medium()` set real weights. `views/kit.rs` is the component kit every
  page builds from (text, sections, cards, callouts, code blocks, key-value, stats, badges, bars, tables,
  virtualised two-line rows, search box, chips, segmented control, section nav, page frames, progress).
  Shell: `views/rail.rs` (left navigation: Home, History, Review, Actions, Ship, Exposure, Insights,
  Govern, Tools; Activity and Settings at the bottom; a dot only when something needs attention, which
  each DevOps area reports through its `rail_badge`), `views/toolbar.rs` (repo tabs + git actions
  only), `views/statusbar.rs` (location, working tree, last activity). The refs sidebar shows on the
  History page only; Exposure and Insights use `kit::page_with_nav` (grouped section navigation).
- Brand (2026-10-29): the product is **Hadion** (it was called gitvis until then; crates, binary, data
  folders, `HADION_*` variables and the keychain service were all renamed). `docs/brand` holds the
  official logo set, colours and rules (Tile below 24 px, light versions on light backgrounds, never
  redraw the mark). In the app: `views/brand.rs` (the Tile at the top of the rail and as the window
  icon, the lockup on the welcome screen and About; PNGs from `crates/hadion/assets/brand`, scaled once
  to the exact pixel size because wgpu has no mipmaps), `build.rs` embeds `hadion.ico` plus version
  details through `winresource` (`windres` on gnu, `rc.exe` on MSVC; a release build fails without
  it). The UI accent stays blue: green means success. Data from before the rename carries over:
  `packaging::carry_over_from_gitvis` copies `%APPDATA%\gitvis` / `%LOCALAPPDATA%\gitvis` once (not in
  scripted runs), the keychain reads service `gitvis` as a fallback, `gitvis-dismissed` and hooks
  marked `gitvis secret check` (backup `pre-commit.before-gitvis`) are still recognised.
- GitHub specifics: job logs need `gh api --allow-escape-sequences` (gh refuses ANSI output otherwise)
  and are unavailable for running or never-started jobs (HTTP 404 from blob storage). An Actions job's id
  is its check-run id (annotations live there). Check runs of a commit are fetched 400 ms after it is
  selected, so arrow-keying through the graph does not spend the 5,000/h API budget.

## DevOps areas (`crates/hadion-core/src/devops/`, `crates/hadion/src/devops/`)

Everything around the repository: review loop, CI control, releases and deployments, what is running,
governance, the local workbench, GitLab / Azure DevOps, the cross-repository home page. One area =
one core module (no UI) + one app module; `app.rs` only routes (`Action::DevOps`, `Event::DevOps`,
`Event::Console*`) and never needs an area's internals.

| Area (app `devops/X`) | Page | Core `devops::X` |
|---|---|---|
| `home` (global state in `Hadion.home`) | Home (`Page::Home`) | inbox, dashboard rows |
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
  `HADION_GH` overrides the `gh` executable (a recording test double).
- Views: `views/form.rs` (inputs with stable ids, `field`, `row`, `checkbox`, `radio`, `select`, `tags`,
  `primary` / `button` / `danger`, `actions_row`, `card`, `error`), `views/console.rs` (`output`,
  `lines`, `run_list`; `ConsoleAction` stop / rerun / copy / remove).
- Rules: secret values only on standard input (`gh secret set NAME` reads stdin; `Request::withheld`),
  never in argv, logs, toasts or errors; masked inputs cleared after sending. Network calls only on an
  explicit action or when the user opens the page / tab that needs them. Writes are pure request
  builders with unit tests. Never run a write against a real remote, registry or cluster while
  testing (brave-core, your own GitHub repositories and local kube contexts are read-only test
  beds); test writes through their request builders and stop UI flows at the confirmation dialog.
  `kubectl` always gets an explicit `--context`; never change the user's current context or kubeconfig.
- Script commands: anything unknown goes to `devops::script` → each area's `script`. Built in: `page
  home|graph|review|actions|ship|exposure|insights|govern|tools`, `ship TAB`, `ship-tab TAB`.
- Round 2 (2026-10-27): more areas on the same framework.
  - Repository areas (state in `RepoState::devops`): `conflict` (merge conflict editor; replaces the graph
    while `conflict::is_open`, opened from a conflicted file in the WIP panel), `history` (Compare page
    `Page::Compare`, graph multi-select in `RepoState::multi`, filters, file tree, inline blame, signature
    verification), `gitplus` (Tools tabs Submodules / Git LFS / Patches / Hooks via `workbench::Tab::gitplus`,
    stash selected files, split a commit, `hadion hook pre-commit` through `gitplus::cli_main` in `main.rs`),
    `ai` (Claude buttons through the owner's Claude Code CLI or the Claude API; config in `Settings::ext.ai`). `devops::on_snapshot` runs after every load.
  - App-wide areas (`devops/global.rs`: state in `Globals`, prefs in `Settings::ext: ExtPrefs`; contract
    `apply(cx, &mut State, &mut GlobalCx, act)`, `on_msg`, `tick` every frame, `script`; `GlobalView` for
    views): `palette` (Ctrl+K overlay), `identity` (git config, identities, keys and signing, GitHub
    account), `terminal` (bottom panel, Ctrl+`), `automation` (notifications, repository groups, Jira /
    Linear links, release helpers), `packaging` (updates, crash reports; `install_crash_handler` in `main`).
    `home` moved into `Globals` too.
  - Settings page (`Page::Settings`, the rail's gear, Ctrl+,): `devops/settings_page.rs` owns the nav and
    Appearance / Behaviour / Safety; other sections call the owning area's `settings_section`.
  - Extension hooks (`devops/hooks.rs`, `HookCx::of(repo, settings)`): commit menu, commit panel, commit
    file menu, branch / remote branch / tag menus (`MenuCtx::hook`, graph and sidebar), WIP file menu,
    WIP message tools, multi-selection panel, job log viewer (`LogHookCx`, which carries only the AI
    state because the CI state is borrowed), pull request conversation, new PR form, status bar.
    Areas contribute through their `hook_*` functions instead of editing shared views. `HookCx` also
    carries the commit message being written and `RepoState::details` (loaded commit details).
  - The WIP commit box takes last frame's height (`wip_commit_box_h`), so hook rows never push the
    Commit button off the panel.
  - Keyboard ownership: the terminal's `tick` runs before `app.rs::shortcuts` and takes every key
    while its grid has focus (only Ctrl+` passes); while the palette is open `shortcuts` returns after
    the palette toggle. App-wide DevOps actions run without an open repository (welcome screen).
  - Two areas keep process-wide state behind a `Mutex` because the views that need it cannot reach
    `Globals`: `automation/shared.rs` (groups runtime, release section, lookups, used by the Home and
    Ship views) and `ai/session.rs` (key status, usage, script overrides; repository areas have no
    app-wide state). Lock briefly, never inside a background task.
  - Safe mode (`Settings::safe_mode`, `hadion_core::devops::safety`): refuses remote writes at the lowest
    level (`cli::run_raw` / `run_program`, `proc::run_streaming` / `run_capture`, so also `api_request` and
    `Gh::cli`), classified by `safety::remote_write(program, args, stdin)`. Task scripts and the
    terminal are not covered; `gh auth switch` / `setup-git` and curl to the Claude API are allowed
    (local config / sends code but changes nothing remote).
  - `hadion_core::devops::keychain` keeps API keys and tokens in the OS keychain (service `hadion`), never
    in settings files or logs; call it from background tasks.
  - ConPTY (Windows pseudo-terminal, `portable-pty`) sends `ESC[6n` first and blocks until the terminal
    answers with the cursor position (`ESC[row;colR`): the terminal must answer it, or nothing appears.
  - Dependencies added to `hadion-core`: `keyring`, `portable-pty`, `vt100`, `notify-rust`, `sha2`.
  - `Action::AddToCommitMessage { text, at_start }` edits the active repository's commit message for
    app-wide areas (which only read repositories); the Tools page has a Stashes tab drawn by `gitplus`.
  - All nine round-2 areas were merged into `devops` on 2026-10-28.
- Per area (core / app files):
  - `review`: core `model`, `graphql` (one list query with mergeability; on a timeout it retries without
    it, since brave-core takes ~7 s per 20 PRs), `rest` (request builders and a `Step` runner that
    substitutes `{number}` of a PR created a step earlier), `patch` (GitHub's bare hunks, comment
    anchors, suggestions), `local` (templates, commits between branches, `#N` matching), `fetch`; app
    `reads`, `changes`, `control`, `view`, `conversation`, `files`, `forms`, `issues`, `widgets`.
    `cargo test -p hadion-core --test review_live -- --ignored --nocapture` is a read-only live check
    (`HADION_REVIEW_REPO=owner/name`).
  - `ci`: core `workflow`/`yaml` (dispatch inputs; serde_yaml reads the YAML 1.1 key `on` as `true`),
    `lint` (`HADION_LINT_DIR=<repo> cargo test -p hadion-core --lib lint_report -- --ignored
    --nocapture` prints a report), `history` (stats, flaky), `compare` (log normalisation and diff; it
    skips the "Set up job" preamble, which ends at `Complete job name:`), `act`, `requests`.
    Re-fetches after a change use `Ctx::after` (GitHub needs a moment), not a sleeping read.
  - `ship`: core `deploy` (one GraphQL query for environments and deployments; `protectionRules`
    errors on some repos, so it is REST-only), `dora`, `notes` (next version, local changelog),
    `local`; app tabs Releases, Environments, Where is my commit, DORA.
  - `runtime`: core `packages` (needs `read:packages`; the page shows the `gh auth refresh` fix and
    never runs it), `kube` (every cluster call has `--context` and `--request-timeout`, except
    `logs -f`, which Stop ends), `terraform` (plan / drift use `-detailed-exitcode`, so their
    `ProcSpec::ok_codes(&[2])` makes the console show exit 2 as passed), `matching` (image tag →
    commit / tag / branch / PR), `fixtures/`.
  - `govern`: core `alerts`, `sbom` (CycloneDX 1.5 / SPDX 2.3 from `osint::parse_manifest`),
    `docker` (Dockerfile parser + lint, scanner JSON), `protection` (the PUT always sends the four
    required keys), `codeowners` (GitHub semantics: last match wins, `docs/*` is not recursive),
    `envs`, `config` (secrets via `gh secret set` stdin, variables).
  - `workbench`: core `tasks/` (discovery; a small order-keeping TOML reader in `toml.rs`;
    `HADION_DISCOVER=<dir> cargo test -p hadion-core --lib print_discovery -- --ignored --nocapture`),
    `rebase` (needs git ≥ 2.38 for `--no-update-refs`; `GIT_SEQUENCE_EDITOR="cp '<todo>'"` and
    `exec git commit --amend -F` lines, so no editor opens), `bisect`, `worktrees`, `history`.
  - `home` / `forge`: core `home::{notify, search, runs, fetch}` (one GraphQL search for review
    requests, mine, mentions, assigned), `forge::{gitlab, azure, cli, trace, tools}` (`glab api`
    with the URL-encoded project path, `az` + azure-devops extension; GitLab job traces parsed into
    nested sections; public gitlab.com projects readable through curl without signing in).
  - `conflict`: core `markers` (git's rules: merge / diff3 / zdiff3, `conflict-marker-size` via
    `check-attr`, look-alike lines stay text, CRLF and a missing final newline round-trip; ambiguous
    splits settled by a diff3 `merge-file` of the stage blobs run with longer markers), `resolve`,
    `load` (`cat-file --batch`, 8 MB cap, NUL in the first 8,000 bytes = binary), `labels` (merge /
    rebase (sides reversed) / cherry-pick / revert / am), `actions`; app `session`, `view`, `panes`
    (one shared scroll), `cards` (non-text conflicts). `scripts/make-conflict-repos.sh DIR` builds
    merge / diff3 / zdiff3 / rebase test repositories.
  - `history`: core compare (`diff --raw --numstat -M -z`), tree, blame (`blame -w -M -C
    --porcelain`), filters (streamed capped `log --all`), signatures (SSH signatures without an
    allowed-signers file read from the commit object: "can't check", not "unsigned"), cherry-pick /
    format-patch plans; app `picks` (multi-select), `sigs` (lazy batch verification of visible rows),
    `widgets`. `cargo test -p hadion-core --test history_git`.
  - `gitplus`: core `stash`, `submodules`, `lfs`, `patches`, `githooks`, `secret_hook`, `precommit`,
    `split`; app tabs on the Tools page (re-exporting `workbench::ui` helpers). `git stash show`
    mangles `--src-prefix`, so `diff.noprefix` / `diff.mnemonicPrefix` are overridden with `-c`.
    git ≥ 2.38.1 refuses file-path submodule URLs (`protocol.file.allow`; the tests pass `-c
    protocol.file.allow=always`). `hadion hook pre-commit` exits 1 on an undismissed high or
    critical staged finding; dismissals are synced to `<git dir>/hadion-dismissed`.
  - `ai`: curl `--config -` on stdin (headers, key, `@body.json`; the body file is deleted on every
    outcome and stale ones swept), SSE parser, redaction through the exposure rules, budgets,
    prices (estimates), refusal / `max_tokens` / fallback handling; the answer streams into an
    `Arc<Mutex<Live>>` the views read. `HADION_AI_KEYCHAIN_NAME` (only `hadion-test-…`), a fake
    curl first on PATH; `--test ai_curl` (Windows) and `--test ai_inputs`. Claude Code route (`cli.rs`):
    `find` (`HADION_CLAUDE`, `~/.local/bin/claude.exe`, PATH, the npm shim's
    `node_modules/@anthropic-ai/claude-code/bin/claude.exe`; never the `.cmd`, whose batch arguments cannot
    carry a multi-line system prompt), `detect` (`--version` + `auth status`, cached five minutes), `args`,
    `env_plan` (removes `ANTHROPIC_*` / `CLAUDE*`, so the plan's sign-in is used), one fixed empty folder
    `%TEMP%\hadion-claude-code`, `Stream` (stream-json; `tools` must be empty), Stop = `kill_tree`.
    `total_cost_usd` is an estimate: covered by a plan, billed for an API-key sign-in. The commit and
    failed-job answers open in windows. Test double: `cargo build -p hadion-core --example fake_claude` +
    `HADION_CLAUDE=<exe>` (`FAKE_CLAUDE_AUTH` plan|env_key|api_key|none, `FAKE_CLAUDE_SCENARIO` /
    `FAKE_CLAUDE_SEQ`, `FAKE_CLAUDE_LOG`); `--test ai_cli`. Never run `claude auth login/logout`,
    `setup-token` or `update` from Hadion or tests.
  - `palette`: core `fuzzy` (alignment + quality filter), `frecency`, `scope`, `item`, `sources`,
    `rank` (narrowing search while the query grows), `files`; app `commands`, `verbs`, `sources`
    (cached by stamp), `results`, `view`. `cargo test -p hadion-core --lib palette_perf -- --ignored
    --nocapture`.
  - `identity`: core config listing with file:line, includeIf rules and a git-compatible wildmatch,
    ssh-keygen / ssh-add / `ssh -T` / gpg colon / `gh auth status` parsers, signing plans; four
    Settings sections. `HADION_GPG`, `HADION_SSH_KNOWN_HOSTS` (Windows ssh ignores HOME, and
    `ssh -T github.com` would write your real known_hosts); test edits with `GIT_CONFIG_GLOBAL`
    and a scratch `GNUPGHOME` (a plain `gpg` against the real home creates `~/.gnupg/trustdb.gpg`).
    `--test identity_apply`.
  - `terminal`: core `session` (spawn returns at once; threads for PTY, reader, waiter, input),
    `stream` (answers ConPTY / terminal queries, keeps SGR 2 dim), `screen` (own scrollback:
    vt100 0.15 panics in debug once scrolled more than one screen up), `keys`, `shells`; app
    `panel`, `input`, `settings`. `--test terminal_pty` runs a real ConPTY.
  - `automation`: core `notify` (poll + diff: the first check only takes note), `groups`, `links`
    (no UTF-8 / SHA-256 false positives; lower-case keys only in branch names), `lookup` (curl
    stdin config), `release` (manifest bumps keeping formatting, Keep a Changelog); app
    `notifications`, `groups` (Home tab), `integrations`, `release` (Ship publish form section).
  - `packaging`: core `version`, `buildinfo` (`crates/hadion/build.rs` embeds commit, dirty, date,
    target, profile, rustc, owner/repo), `release`, `checksum`, `zip`, `update`, `swap` (rename to
    `.old`, move `.new` in, relaunch, roll back; refuses exes inside a Cargo target folder: never
    run it on `target/debug` or `target/release`), `crash` (panic hook, redacted reports), `issue`,
    `paths`; `scripts/package.ps1`, `packaging/windows/hadion.iss`, `.github/workflows/{ci,release}.yml`.
- Test doubles: `HADION_GH` (any area), `HADION_KUBECTL`, `HADION_TERRAFORM` replace the executables.
  `cargo build -p hadion-core --example gh_guard`, then `HADION_GH=target/debug/examples/gh_guard.exe`
  passes reads to the real gh and blocks every write (non-GET, GraphQL mutations, other subcommands),
  logging to `GH_GUARD_LOG`; `GH_GUARD_FAIL=msg` forces error states. Use it for every UI session
  that touches GitHub. Read-only public test beds the area agents used: cli/cli (environments, 3.6k
  deployments, releases), pallets/click (releases with assets), sharkdp/fd (small artifacts),
  gitlab-org/gitlab-runner (public GitLab API); brave-core has no releases or deployments.
  Round 2: `HADION_NOTIFY=log|off` + `HADION_NOTIFY_LOG` (scripted runs always log, never show a
  notification), `HADION_CURL` (issue lookups), `HADION_KEYCHAIN_TEST=1` (`hadion-test-*` entries),
  `HADION_UPDATE_DIR` / `HADION_UPDATE_THROTTLE_KIB` / `HADION_SCRIPT_AFTER_UPDATE` (updates from a
  folder), `HADION_DATA_DIR` / `HADION_CRASH_DIR`, `HADION_ALLOW_CRASH_TEST=1` (with a script, for
  `crash-test`), `HADION_CLAUDE` (a fake Claude Code CLI). Never call the real Claude API while testing (it
  costs money and sends code).
- Script commands per area (each area's `script` fn has the full list): review `review`, `prs`,
  `issues`, `pr N`, `pr-tab conversation|files|commits|checks`, `pr-file N`, `issue N`, `new-pr`,
  `pr-lines A B`, `pr-comment TEXT`, `pr-review approve|changes|comment TEXT`, `pr-submit` /
  `pr-merge` / `create-pr` (stop at the confirmation); ci `actions-tab runs|workflows|artifacts|
  caches|validate|local`, `workflow N`, `dispatch N`, `select-runs N`, `compare`, `analyse-jobs`;
  ship `ship releases|envs|where|dora|packages|k8s|terraform`, `release N`, `new-release`, `where
  SHA|head`, `env N`, `dora-source …`, `dora-period N`; runtime `k8s-context NAME`, `workload N`,
  `pod-logs N`, `tf-root N`, `tf-validate`, `pkg N`; govern `gov-tab dependabot|code-scanning|sbom|
  images|protection|codeowners|environments|secrets|variables`, `gov-item N`, `codeowners-test PATH`,
  `secret-form`; tools `tools-tab tasks|console|rebase|bisect|history|worktrees`, `select-task N`,
  `rebase BASE`, `bisect`, `history PATH`, `worktree N`; home `home`, `home-tab …`, `inbox-item N`,
  `forge-tab review|pipelines`, `mr N`, `pipeline N`. Round 2: conflict `conflict PATH`,
  `conflict-next|prev|next-file`, `conflict-hunk N ours|theirs|both|both-theirs|base|reset`,
  `conflict-all ours|theirs`, `conflict-base on|off`, `conflict-edit N`, `conflict-type TEXT`,
  `conflict-save`, `conflict-resolve-ask`, `conflict-choice …`, `conflict-recreate STYLE`,
  `conflict-raw`, `conflict-continue`, `conflict-close`; history `hist-compare A B|worktree`
  (`compare A B` too), `compare-mode two|three`, `compare-tab files|commits`, `hist-mode
  compare|files|blame`, `pick-commits N`, `multi-action …`, `filter-author|path|since|until|message
  TEXT`, `filter-clear`, `browse SHA|head`, `browse-file PATH`, `blame-at REV PATH [LINE]`,
  `verify`; gitplus `stashes`, `stash N`, `stash-form`, `stash-files P1,P2 MSG`, `submodules`,
  `lfs`, `patches`, `patch-preview PATH`, `hooks`, `hook-view NAME`, `install-secret-hook`,
  `split-commit SHA|head`; ai `ai-commit`, `ai-send`, `ai-pr`, `ai-explain [RUN JOB]`, `ai-summary
  [N]`, `ai-commit-explain SHA|head`, `ai-model NAME`, `ai-ask on|off`, `ai-provider auto|cli|api`,
  `ai-cli-model default|opus|sonnet|haiku|fable`, `ai-cli-check`, `ai-settings [usage|cli|api]`; palette `palette-query
  TEXT`, `palette-down|up N`, `palette-run`, `palette-actions`, `palette-close`; identity
  `config-tab …`, `config-edit KEY VALUE [scope]`, `identity-form …`, `check-path PATH`,
  `ssh-test`, `signing-form ssh|gpg`, `upload-key N`, `gh-switch …`; terminal `term-new SHELL`,
  `term-type TEXT`, `term-press KEYS`, `term-scroll N`, `term-select …`, `term-restart`,
  `term-close`; automation `notify-on|off|test|poll|list`, `groups`, `group-new NAME`, `group-add
  NAME PATH`, `group-fetch|pull NAME`, `links-test TEXT`, `links-jira BASE [K,K]`, `release-prep
  VERSION`; packaging `update-check`, `update-download`, `update-confirm-restart`, `crash-open N`,
  `crash-report N`. Commands that would write stop at the confirmation dialog, except
  `rebase-run-now`, `bisect-run-now CMD`, `task N`, `fetch-all-in DIR`, `conflict-resolve`,
  `conflict-choice-now X`, `group-run-now NAME …`, `release-commit-now VERSION` and
  `update-restart` (test mode only), which act for real and are for scratch repositories only.

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
- `ScrollArea::show_rows(ui, row_h, n, ..)` spaces rows by `row_h + item_spacing.y`: set `item_spacing.y = 0`
  BEFORE the call (in a `ui.scope`), not inside the closure, or rows are scrolled further apart than they are
  drawn and the list ends short of its last rows with an empty band (the "black box" in the file viewer).
- `ui.set_max_width(W)` sets the width exactly; it does not cap it. Write
  `ui.set_max_width(ui.available_width().min(W))`, or a narrow window gets a page wider than itself.
- Readability rules learnt the hard way (`views/insights/mod.rs` has the helpers): explanations at 12.5 px
  or more (never `.small()` for prose), prose capped at `PROSE_WIDTH`, section headers with a hairline,
  `why_box` with a caption, 20 px tile values. `.small()` is for badges only.

## Scripted smoke tests (preferred over mouse automation)

`HADION_SCRIPT="wait 10; tab 1; head; open path/in/commit; wait 2; shot out.png; split; shot out2.png; quit"`
drives the app from inside and saves PNGs rendered by egui (works whatever is on screen).
Commands: `wait S`, `shot PATH`, `window W H` (inner size in points; 900 × 560 is the minimum), `dialog-cancel`
(close the open dialog), `tab N`, `head`, `wip`, `open PATH`, `open-wip PATH`, `split`,
`unified`, `select HUNK FROM TO`, `activity`, `reflog`, `collapse KEY`, `stage PATH`, `undo`,
(scripted runs never save settings, window geometry or egui memory, so they cannot change your own
session; run a copy of the exe under another name, e.g. `hadion-test.exe`, so the build is never locked),
`theme`, `github` / `graph` (centre page), `filter TEXT` and `status failed|running|passed|all`
(run list), `run N` (open the N-th listed run), `job N` (show the N-th job's log), `verbose`,
`security` / `scan` / `scan-history N` / `finding N` / `reveal` (Exposure page), `sec-tab NAME` /
`osint` / `item N` / `unreachable` / `osv` / `dorks` / `pr-refs` (OSINT tabs), `insights` /
`ins-tab NAME` / `analyse N` / `ins-item N` / `weight` / `date-debt` / `pickaxe TEXT` / `grep TEXT` /
`recover` / `blame PATH` (Insights page), the DevOps commands (see "DevOps areas"), `quit`. See
`crates/hadion/src/script.rs`. `stage`/`undo` run real git operations; the GitHub commands make
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
then `ins-tab ci; ins-item 0`. A scripted window is a real window: clicks in it (for example
while you work in your own Hadion instance on the same desktop) can change what it shows, so a
screenshot may show a different tab than the script set. Scripted runs also restore the tabs
saved by your own Hadion session; `HADION_NO_RESTORE=1` opens only the repository on the command
line (README screenshots are taken that way).

## UI testing without a human

`scripts/make-demo-repo.sh <dir>` builds a demo repository (branches, merges, tags, remote, stash, dirty tree).
`scripts/ui-shot.ps1 -Out x.png` screenshots the Hadion window; `scripts/ui-input.ps1 -Action click|rclick|
dblclick|drag|scroll|keys -X .. -Y ..` sends input in window-relative coordinates (same space as the
screenshots). Launch `target/debug/hadion.exe <repo>` in the background first; the exe is locked while it runs,
so stop it before rebuilding.
