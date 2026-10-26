# gitvis

A fast, native Git client focused on **branch visualisation and management** —
an in-house alternative to GitKraken, written in Rust with [egui](https://github.com/emilk/egui).

- Reads repositories through **libgit2** (in-process, no shelling out for the graph).
- Runs every mutation through the **real `git` binary**, so hooks, signing,
  credential helpers, LFS and your config behave exactly as on the command line.
- Handles large histories: 50 000 commits are laid out in well under a second;
  rows are virtualised, so scrolling a 100k-commit repository stays smooth.
- Refreshes automatically when the working tree or `.git` changes.

## Features

**Graph**
- Lane layout with colour-stable branches, merge rings, HEAD marker and a dashed
  "uncommitted changes" row.
- Ref pills on commits (local / remote / tag), with overflow folding.
- Search commits, authors, SHAs and ref names (`Ctrl+F`, `Enter` / `Shift+Enter` to step).
- Keyboard navigation (`↑` / `↓`), click to inspect, right-click for actions.
- Hide individual refs from the graph, show a single branch ("solo"), hide all remotes or tags.

**Branches & refs**
- Sidebar tree for local branches, remotes, tags and stashes with a filter box.
- Create / rename / delete branches, create and delete tags, push tags.
- Checkout local branches, check out remote branches as tracking branches.
- **Drag a branch onto a commit or branch** to merge or rebase.
- Merge (default / no-ff / ff-only / squash), rebase, cherry-pick, revert, reset (soft / mixed / hard).
- Fetch, pull (merge / ff-only / rebase), push with upstream setup and `--force-with-lease`.
- In-progress merge / rebase / cherry-pick / revert banner with Continue, Skip, Abort.

**Working tree**
- Staged / unstaged / conflicted lists with per-file stage, unstage, discard, delete.
- Conflict resolution: keep ours, take theirs, mark resolved.
- Commit, amend, stash (with untracked), apply / pop / drop stashes.

**Diff viewer**
- Unified or **side-by-side**, syntax colouring (syntect), **word-level change emphasis**.
- Stage / unstage / discard **per hunk or per selected lines** (click, shift-click, drag);
  patches are built byte-exact and applied with `git apply`.
- Ignore-whitespace toggle, adjustable context, copy line / selection / hunk, open file,
  reveal in Explorer.

**Activity, reflog, undo**
- Activity panel (`Ctrl+L`): everything gitvis did, with the exact command, the full
  output, working directory, size and duration. Filter by kind (operations, repository
  reads, GitHub API) or text; copy commands or output.
- **Verbose mode** (on by default, gear menu): background reads are logged too — what a
  repository load found (commits, refs, status, lanes, time), every `git diff` with its
  hunk/line counts, every `gh api` request with bytes, latency and the remaining rate limit.
- Reflog tab with old → new SHAs, exact times, jump-to-commit, branch-from-here and hard reset.
- **Undo** (`Ctrl+Z`): derived from the reflog — undo commit/amend (soft reset keeps the
  changes staged), checkout, merge, pull, rebase, reset, cherry-pick, revert. Destructive
  undos ask first.
- Status bar: HEAD and upstream, counts, load time, working-tree state, GitHub summary,
  API budget, and the last thing that ran.

**Home** (`Ctrl+Shift+H`): every open repository at a glance and your GitHub inbox
- One card per open repository: branch with ahead / behind, staged / changed / untracked /
  conflicted counts, stashes, CI on HEAD, open pull requests and yours, failed runs on your
  branches, urgent exposure findings, operations in progress. **Fetch all** runs
  `git fetch --all --prune` in each repository after a confirmation listing them.
- **Inbox**: notifications by reason (review requested, mention, assigned, CI, security, …)
  with unread / open-in-gitvis / group-by-repository filters, plus review requests, your
  pull requests, mentions and assignments from one GraphQL search, and failed runs on your
  branches from the data already loaded. Items open in gitvis when their repository is open.

**Review** (`Ctrl+Shift+R`): pull requests and issues (through the `gh` CLI, so gitvis stores no token)
- Pull request list from one GraphQL query: draft, review decision, checks, conflicts or
  behind, your review requested, labels in their colours; chips for open / draft / review
  requested / mine / closed, search, paging.
- Conversation with the description, timeline and a comment box, and a **merge box**:
  mergeability explained, only the merge methods the repository allows, commit title and
  message, delete branch, **auto-merge**, update branch, ready for review / draft,
  close / reopen, check out locally.
- **Review in the diff**: changed files with viewed marks, syntax-coloured patches with both
  line numbers, review threads under their lines (resolve / unresolve, reply), select a line
  or range to comment, **suggest a change**, pending comments submitted as one review
  (comment / approve / request changes). Commits and checks tabs.
- **New pull request** inline: base and head, push state ("push first"), PR template (or a
  chooser), a body generated from the commits, reviewers, teams, assignees, labels, draft.
- **Issues**: list with filters and labels, detail with comments, comment / close (completed
  or not planned) / reopen, edit labels and assignees, new issue with templates, **start a
  branch** `N-slug`, "Fixes #N", and the commits in the graph that mention the issue.
- Every change shows the exact `gh api` requests and JSON bodies before it runs.

**Actions** (`Ctrl+Shift+G`): workflow runs and CI control
- **Runs**: the latest 100 runs with status, title, workflow, event, branch, actor, age and
  duration; filter by text or status; pick several for **bulk cancel / re-run**; per run
  re-run all or failed jobs (optionally **with debug logging**), re-run one job, cancel,
  force cancel, cancel everything running on a branch, delete logs or the run. Jobs, steps
  with their share of the time, annotations, artifacts, and the **full job log** (foldable
  groups, ANSI colours, timestamps, filter, jump between errors).
- **Compare with the last green run**: commits and files in between, and a normalised log
  diff of each failing job (first divergence, lines only in the failing run, hunks).
- **Workflows**: every workflow with its last 100 runs, success rate, median and p90
  duration, queue time, a duration trend chart, runs by branch and event, flaky signs, and
  on demand a per-job analysis across attempts; enable / disable; **run a
  workflow_dispatch workflow** with a form built from its typed inputs.
- **Artifacts** (sizes, expiry, download into a folder, delete) and **caches** (usage against
  the 10 GB limit, by key or ref, delete, clean up caches of closed pull requests).
- **Validate**: lint every workflow file (YAML errors, unknown events and contexts, broken
  `needs`, cycles, unbalanced expressions, deprecated actions and commands, unpinned actions,
  `pull_request_target` pitfalls, missing permissions and timeouts, cron, dispatch inputs)
  with lines and fixes, merged with `actionlint` when installed.
- **Run locally** with `act` (jobs, event, inputs, image, a secrets file by path).
- **Checks on any commit** in the details panel (check runs and commit statuses), the CI dot
  in the graph, and "Open on GitHub" on commits, branches and runs.

**Ship** (`Ctrl+Shift+D`): releases, deployments and what is running
- **Releases**: list with assets and download counts, download an asset, edit, delete, and
  **publish** a release with a suggested next version (from conventional commits), notes
  generated by GitHub or grouped locally, draft / pre-release / latest, and assets
  (`gh release create`).
- **Environments**: what is deployed where (commit, ref, when, by whom, commits behind the
  default branch, what staging has that production does not), deployment history, **pending
  approvals** (approve / reject with a comment), **promote** and **roll back** by re-running
  the deploying run or creating a deployment.
- **Where is my commit**: authored and committed, the pull request that brought it in,
  branches, the first tag and releases that contain it, checks, and each environment it has
  reached (and since when). Also in the commit context menu.
- **DORA metrics** from deployments, releases, tags or workflow runs over 30 / 90 / 180 days:
  deployment frequency, lead time for changes, change failure rate and time to restore, with
  performance bands, weekly charts, the method and the data coverage.
- **Packages**: container images on GitHub Packages with their tags matched to commits,
  tags, branches and pull requests, `docker pull` lines, delete a version (needs the
  `read:packages` / `delete:packages` scopes; the page shows the `gh auth refresh` command).
- **Kubernetes** through `kubectl` (any context, never switched): workloads with rollout
  state and the commit each image runs, pods with restarts and last termination, events,
  rollout history, streamed logs; restart, undo and scale after a confirmation.
- **Terraform / OpenTofu**: roots and modules, the roots your changes touch, fmt check,
  init (`-backend=false`), validate with file and line, plan with a resource table, and
  drift checks. Nothing is ever applied.

**Govern** (`Ctrl+Shift+A`): security and administration
- **Dependabot and code-scanning alerts**: severity tiles, details (CVSS, CWE, EPSS),
  fix guidance from the local lockfile, the Dependabot pull request, the flagged code;
  dismiss with a reason or reopen.
- **SBOM** from the lockfiles, exported as CycloneDX 1.5 or SPDX 2.3, compared with
  GitHub's dependency-graph SBOM.
- **Container images**: Dockerfile lint (unpinned or `latest` bases, root user, `ADD` URLs,
  `curl | sh`, package-manager hygiene, secret-looking `ENV` / `ARG` names, cache busting, …),
  compose images, and scans with `trivy`, `grype` or `docker scout` in the console.
- **Branch protection** as a form (reviews, status checks, linear history, force pushes,
  conversation resolution, lock), rules in effect and rulesets.
- **CODEOWNERS**: GitHub's matching (last rule wins), validation, GitHub's own errors, rules
  that never decide or match nothing, "who owns this path", coverage and unowned
  directories, and an editor.
- **Environments** (reviewers, wait timer, branch policies), **Actions secrets** (values go
  only to `gh secret set` on standard input and are never shown or logged) and
  **variables**, per repository and environment.

**Tools** (`Ctrl+Shift+T`): the daily workflow
- **Tasks** discovered from package.json (with the right package manager), Makefile,
  justfile, Taskfile, Cargo, composer, deno, Go, .NET, Gradle, Maven, Python and docker
  compose; run them in the console with extra arguments, history and favourites.
- **Console**: every streamed process (tasks, `act`, `kubectl logs`, `terraform`, scans,
  `git bisect run`) with its command, status, duration, Stop and Run again.
- **Interactive rebase** without an editor: pick / reword / edit / squash / fixup / drop,
  reorder by drag or keys, combined squash messages, a preview and a warning for pushed
  commits. Also "Interactive rebase from here…" in the graph.
- **Bisect** by hand (good / bad / skip) or with a test command (`git bisect run`), with the
  steps left, the tested commits and the first bad commit.
- **Worktrees**: list with state, open as a tab, add, lock, move, remove, prune.
- **File history** across renames, with each commit's diff of the file; also "File history"
  in the file menus.

**GitLab and Azure DevOps**
- Repositories whose remote is on GitLab (`glab api`, self-hosted hosts included) or Azure
  DevOps (`az` with the azure-devops extension) get the Review and Actions pages for merge
  requests / pull requests (create, merge or complete, approve, vote, comment) and
  pipelines (jobs, stages, foldable logs, retry, cancel, run with variables). Public
  gitlab.com projects can be read without signing in.

**Exposure: is the repository leaking anything?** (`Ctrl+Shift+S`, or the Exposure button)
- **Secrets in content**: 50+ patterns (AWS, GitHub, GitLab, Slack, Stripe, Google, OpenAI,
  Anthropic, npm, PyPI, Azure, private keys, JWTs, passwords in URLs, credentials in
  `.env`/ini/yaml, generic `password =` assignments, high-entropy strings) with placeholder
  and entropy filters; a `gitvis:allow` comment on a line skips it.
- **Sensitive files**: `.env*`, SSH and PGP keys, `*.pem/.key/.p12/.pfx/.jks`, credential
  stores (`.git-credentials`, `.netrc`, `.npmrc`, `.pypirc`, `.aws/credentials`, kubeconfig,
  Docker config), Terraform state and variables, database dumps, shell histories, …
- **Where it is**: in the working tree, **staged** (would go into the next commit), in
  **HEAD**, or **only in history** (committed and removed again, still in every clone);
  the commit that introduced it (`git log -p -U0`, added lines only, newest N commits or
  the entire history, HEAD or all refs); and whether it was **pushed** (reachable from a
  remote-tracking branch). Occurrences link to the diff or the commit.
- **Configuration**: passwords embedded in remote URLs (one click to strip them), plaintext
  credential helper, disabled TLS verification, missing `.gitignore`.
- **GitHub**: public or private, forks, whether secret scanning and push protection are on,
  and open secret-scanning alerts.
- **What to do**: per finding, numbered steps with buttons: open the provider's key page,
  open or unstage the file, stop tracking (`git rm --cached`), add to `.gitignore`, copy a
  `git filter-repo` command to purge it from history, dismiss as a false positive
  (remembered per repository).
- **Before every commit** the staged changes are checked; flagged files get a red shield in
  the working-tree list, and the commit button asks before committing a possible secret.
- Secrets never appear on a command line or in the activity log; the log records counts,
  paths and the git commands that ran. Whole-tree scans run on all cores and are gated by
  keyword matching, so a 500 MB tree takes seconds; history is read with `git log -p -U0`.

**Git OSINT: what the repository tells an outsider** (tabs on the Exposure page)
- **People**: every author and committer address with its kind (personal mailbox, corporate
  domain, GitHub noreply, invalid), the names used with it, aliases (same name, other
  addresses), commits authored and pushed, first and last activity, UTC offsets, and a
  weekday × hour heat map per person and for the whole team; warns when your configured
  `user.email` would go into a public repository.
- **Footprint**: infrastructure and personal data named in the files, found with the same
  engine as the secrets but other rules: private and public IPs, internal hostnames, AWS
  account IDs, S3 buckets, GCP projects, Azure subscriptions, private container registries,
  Slack workspaces, local paths with user names, e-mail addresses, phone numbers, IBANs
  (checksum-verified), payment cards (Luhn-verified), US social security numbers.
- **Files**: EXIF in JPEG, PNG and TIFF (GPS position with a map link, camera, owner, serial
  numbers), PDF Info and XMP, Office and OpenDocument properties (creator, last modified by,
  company), absolute build paths inside committed binaries, and junk files from developer
  machines (`.DS_Store`, swap files, logs, crash dumps, IDE workspaces).
- **History**: commit messages that describe security events (removed secrets, leaks,
  disabled checks, hacks, accidents), commits only the reflog still reaches (with a one-click
  check whether GitHub still serves them), stashes with their secrets, backup-looking
  branches, deleted files with sensitive names (with the command that resurrects them),
  `git fsck --unreachable` on demand with a secret scan of the unreachable blobs, the
  pull-request refs the remote advertises, and trailers, ticket keys and URL domains.
- **GitHub**: visibility, forks (and why forks keep purged commits), secret-scanning,
  Dependabot and code-scanning alerts, the admin surface (deploy keys, webhooks,
  environments, Actions secret names, variables, branch protection, collaborators),
  CODEOWNERS, and twelve code-search **dorks** run against the owner with your own account.
- **Supply chain**: dependencies pinned to git URLs (package.json, Cargo, pip, go.mod,
  Gemfile, pyproject), packages from lockfiles checked against **osv.dev** on demand,
  committed hooks (husky, pre-commit), filter and diff drivers, LFS configuration.
- Everything but osv.dev, GitHub and `ls-remote` is local; those run only when asked.

**Insights: what git knows but no client shows** (`Ctrl+Shift+I`, or the Insights button)
- **Health**: checkout landmines (case collisions, Windows reserved names and forbidden
  characters, trailing dots, paths over 260 bytes, Unicode normalisation clashes, symlinks,
  tracked-but-ignored files, huge directories, nested repositories), line endings from
  `git ls-files --eol` (CRLF/LF/mixed, files whose working tree fights the index, BOMs,
  non-UTF-8, misattributed binaries, autocrlf and `.gitattributes` state), executable bits
  (scripts without `+x`, data files with it), repository weight (`count-objects`, largest
  HEAD blobs, bytes by directory and extension, LFS pointers and candidates, commit-graph,
  shallow/partial clone, worktrees, loose vs packed refs, gc advice) and, on demand, every
  object in history (`rev-list --objects --all` through `cat-file --batch-check`: biggest
  blobs ever committed, dead weight no longer in HEAD), plus submodule state.
- **Hotspots**: files ranked by recency-weighted churn with bug-fix share and author count,
  temporal coupling (files that keep changing in the same commits, cross-directory pairs
  marked) and code age per directory, from one streamed `git log --numstat -M` pass.
- **Owners**: lines added per author per directory, bus factor, orphaned directories (one
  owner, gone for a year), and `git blame` of any file on demand (surviving lines by author
  and by year).
- **Branches**: stale, merged, gone and throwaway branches, ahead/behind, unique commits,
  predicted merge conflicts (files both sides changed since the merge base), patches already
  present elsewhere (`git cherry`), switch/delete from the detail.
- **Commits**: hygiene (WIP and fixup commits, reverts and reverts of reverts, giant commits,
  conventional-commit adoption, issue references, merge vs rebase style by year) and
  provenance (signature verification of the newest commits via `%G?`, bots, author ≠
  committer, future / epoch / rebased / backdated / offset-jump dates).
- **Releases**: tags (annotated, signed, orphan), unreleased commits grouped as a changelog
  draft, cadence, version bumps, suggested next version from conventional commits.
- **Timeline**: commits and lines per month with joins and departures, contributors with
  spans, language breakdown, and when work happens (weekday × hour).
- **Debt**: TODO / FIXME / HACK / XXX / BUG / NOCOMMIT markers by directory and assignee,
  dated on demand through `git blame` (who wrote each, how long ago, oldest first).
- **CI**: GitHub Actions workflows parsed for unpinned third-party actions,
  `pull_request_target` with a PR checkout, script injection through `${{ github.event.* }}`,
  missing or `write-all` permissions, secrets passed to third parties, self-hosted runners on
  fork-triggerable workflows, `curl | sh`, missing timeouts, deprecated commands; plus
  secrets, runners, other CI systems, Dependabot and Renovate.
- **Licences**: the project licence (classified), manifest declarations that disagree with
  it, dependency licences found offline (package-lock, node_modules, the local cargo
  registry, vendored trees), copyleft and AGPL dependencies, copyright headers naming other
  holders (copied code).
- **Search**: pickaxe (`git log -S`), diff regex (`-G`), message and author search across
  all refs, with the files each hit touched and a one-click diff.
- **Recovery**: commits and stashes no ref reaches (`git fsck --unreachable --no-reflogs`),
  with reflog provenance and changed files; restore as a branch, cherry-pick, or
  `git stash store`.

**Commit details**
- Author and committer with e-mails and both timestamps (and the delay between them),
  full SHA and tree SHA, signature presence (GPG / SSH / X.509), parents, refs, and
  "in branches" (`git branch --contains`, on demand).
- File list with per-kind counts (added / modified / deleted / renamed / binary) and stats.

**App**
- A navigation rail for the pages of the open repository (Home, History, Review, Actions,
  Ship, Exposure, Insights, Govern, Tools), with the activity log and settings at the
  bottom; a dot marks a page that needs attention (failed runs, review requests, approvals
  waiting for you, critical alerts, running tasks, a rebase or bisect in progress).
- Every change made outside git (GitHub, GitLab, Azure DevOps, kubectl, terraform) is
  confirmed first with the exact command or request, and logged with its request body;
  network calls only happen when you open a page or press a button.
- Multiple repositories as tabs; clone (`Ctrl+Shift+O`), open (`Ctrl+O`), or drag a folder
  onto the window.
- Inter and JetBrains Mono (both SIL Open Font Licence, bundled in
  `crates/gitvis/assets/fonts` with their licences), dark and light themes, relative or
  absolute dates.
- Settings and per-repository preferences persist between runs.

## Build and run

Requirements: Rust 1.85+ (edition 2024), a C compiler for libgit2 (MSVC or MinGW on
Windows), and `git` on `PATH` at runtime.

```sh
cargo run --release -p gitvis -- path/to/repo
```

Run without arguments inside a repository to open it, or open one from the
welcome screen (`Ctrl+O`).

Tests:

```sh
cargo test
```

The integration tests build throwaway repositories with the real `git` binary.
`GITVIS_TIMING=1` prints loader phase timings to stderr;
`cargo run --release -p gitvis-core --example bench -- path/to/repo` times a load;
`--example scan -- path/to/repo [--history N]` prints the exposure report,
`--example osint -- path/to/repo [--unreachable]` the OSINT report and
`--example insights -- path/to/repo [--commits N] [--weight] [--lost] [--date-debt] [--pickaxe TEXT]`
the insights report.

## Keyboard

| Keys | Action |
| --- | --- |
| `Ctrl+O` | Open repository |
| `Ctrl+W` | Close current tab |
| `Ctrl+F` | Search commits |
| `Enter` / `Shift+Enter` | Next / previous search match |
| `↑` / `↓` | Move commit selection |
| `Esc` | Clear diff selection, then close the diff view / search |
| `Ctrl+Enter` | Commit (while writing the message) |
| `Ctrl+L` | Activity log / reflog panel |
| `Ctrl+Z` | Undo the last HEAD movement |
| `Ctrl+Shift+O` | Clone repository |
| `Ctrl+Shift+H` | Home: open repositories and the GitHub inbox |
| `Ctrl+Shift+R` | Review page (pull requests, issues) |
| `Ctrl+Shift+G` | Actions page (runs, workflows, artifacts, caches, validation, local runs); `Esc` returns to the graph |
| `Ctrl+Shift+D` | Ship page (releases, environments, DORA, packages, Kubernetes, Terraform) |
| `Ctrl+Shift+A` | Govern page (alerts, SBOM, images, protection, CODEOWNERS, secrets) |
| `Ctrl+Shift+T` | Tools page (tasks, console, interactive rebase, bisect, file history, worktrees) |
| `Ctrl+Shift+S` | Exposure page (secrets, sensitive files, configuration); `Esc` returns to the graph |
| `Ctrl+Shift+I` | Insights page (health, hotspots, owners, branches, commits, releases, timeline, debt, CI, licences, search, recovery) |
| `F5` | Reload repository |

## Architecture

```
crates/
  gitvis-core/   repository model, graph layout, diff parsing, git operations
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
                     workbench/, forge/ (GitLab, Azure DevOps), home/
    tests/           end-to-end tests against generated repositories
  gitvis/        egui application
    src/app.rs       state machine: loads, operations, watcher, actions, verbose logging
    src/worker.rs    background reader thread + one thread per operation or API request
    src/watcher.rs   file-system watcher with .git-aware classification
    src/highlight.rs syntect colouring, computed on the reader thread
    src/script.rs    GITVIS_SCRIPT smoke-test driver (screenshots via egui)
    src/theme.rs     design tokens: palette, type scale, spacing, fonts
    src/views/kit.rs the component kit every page is built from
    src/views/       rail, toolbar, sidebar, graph, details, diff, github, security, osint, insights/,
                     form (inputs), console (process output), activity, …
    src/devops/      one module per DevOps area (state, actions, replies, views), the Ctx
                     command buffer they use, the console, the Ship page navigation
```

The UI never blocks on git: snapshots, details and diffs are loaded on a
reader thread, operations run on their own thread, and results come back as
events. Views emit `Action`s which the app applies after each frame, so views
stay free of orchestration logic. DevOps areas never touch the app directly: their
controllers push effects (GitHub or local reads, changes, console processes, actions) into
a `Ctx`, and their replies come back as messages routed to the same area.

## Status

Early but usable. Not yet implemented: submodules, signature verification beyond `%G?`,
GPG/SSH key management, custom secret rules (the rule set is built in), comments on a whole
file or editing existing review comments, Azure DevOps pull request comments, Bitbucket.
GitHub, GitLab and Azure DevOps writes are covered by request-builder tests and were
exercised up to their confirmation dialogs; they have not yet been run against a sandbox
repository.
