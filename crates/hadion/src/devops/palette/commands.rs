//! The command catalogue: every page and tab, app actions, git actions for
//! the active repository and the actions of the other areas, each with
//! the app actions it runs. Built from the current context (what is
//! available, which way a toggle goes) and rebuilt when that changes.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use hadion_core::Pending;
use hadion_core::devops::palette::sources::CommandEntry;
use hadion_core::ops::GitOp;

use super::Cx;
use crate::devops::settings_page::Section;
use crate::devops::{DevOpsAction, ShipTab, ci, govern, home, review, ship, workbench};
use crate::state::{Action, Dialog, InsightsTab, Page, PullPref, SecurityTab};
use crate::views::icons::Icon;

pub const GO: &str = "Go to";
pub const APP: &str = "App";
pub const GIT: &str = "Git";
pub const GITHUB: &str = "GitHub";
pub const PALETTE: &str = "Palette";

/// One command.
pub struct Command {
    pub entry: CommandEntry,
    pub icon: Icon,
    pub actions: Vec<Action>,
    /// Running it changes the palette itself, which stays open.
    pub keep_open: bool,
}

impl Command {
    fn new(id: &str, title: impl Into<String>, category: &str, icon: Icon) -> Self {
        Self {
            entry: CommandEntry {
                id: id.to_owned(),
                title: title.into(),
                category: category.to_owned(),
                ..Default::default()
            },
            icon,
            actions: Vec::new(),
            keep_open: false,
        }
    }

    fn hint(mut self, h: &str) -> Self {
        self.entry.hint = h.to_owned();
        self
    }

    fn key(mut self, k: &str) -> Self {
        self.entry.shortcut = k.to_owned();
        self
    }

    fn kw(mut self, k: &str) -> Self {
        self.entry.keywords = k.to_owned();
        self
    }

    fn run(mut self, a: Vec<Action>) -> Self {
        self.actions = a;
        self
    }
}

/// The commands available now.
#[derive(Default)]
pub struct Catalog {
    pub cmds: Vec<Command>,
    index: HashMap<String, usize>,
}

impl Catalog {
    pub fn get(&self, id: &str) -> Option<&Command> {
        self.index.get(id).map(|&i| &self.cmds[i])
    }

    pub fn entries(&self) -> Vec<CommandEntry> {
        self.cmds.iter().map(|c| c.entry.clone()).collect()
    }

    fn push(&mut self, c: Command) {
        debug_assert!(!c.actions.is_empty(), "{} runs nothing", c.entry.id);
        self.index.insert(c.entry.id.clone(), self.cmds.len());
        self.cmds.push(c);
    }
}

/// Changes whenever the catalogue would.
pub fn stamp(cx: &Cx) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let s = cx.settings;
    (
        s.dark,
        s.show_terminal,
        s.show_activity,
        s.safe_mode,
        s.verbose,
        s.show_status_bar,
        s.relative_dates,
        s.show_author,
        s.pull as u8,
    )
        .hash(&mut h);
    (cx.active, cx.repos.len()).hash(&mut h);
    if let Some(r) = cx.repo() {
        (r.id, r.github_remote.is_some(), r.forge_remote.is_some()).hash(&mut h);
        r.undo.as_ref().map(|u| (&u.label, &u.detail)).hash(&mut h);
        if let Some(snap) = &r.snapshot {
            snap.head.branch().hash(&mut h);
            snap.head.target().map(|o| o.to_string()).hash(&mut h);
            (snap.pending as u8, snap.stashes.len()).hash(&mut h);
            snap.stashes.first().map(|s| &s.message).hash(&mut h);
            snap.status.is_clean().hash(&mut h);
            snap.remotes.len().hash(&mut h);
            if let Some(b) = snap.current_branch() {
                (b.ahead, b.behind, &b.upstream).hash(&mut h);
            }
        }
    }
    h.finish()
}

/// Build the catalogue for the current context.
pub fn catalog(cx: &Cx) -> Catalog {
    let mut c = Catalog::default();
    let repo = cx.repo();
    let has_repo = repo.is_some();
    let github = cx.github().is_some();
    let remote = repo.is_some_and(|r| r.github_remote.is_some() || r.forge_remote.is_some());
    let s = cx.settings;
    let show = |p: Page| Action::ShowPage(p);

    // ----- Pages -----
    if has_repo {
        let pages: [PageRow; 11] = [
            (
                "page:home",
                "Home",
                Page::Home,
                Icon::Home,
                "Ctrl+Shift+H",
                "Every open repository at a glance and the GitHub inbox",
                "dashboard inbox notifications",
                false,
            ),
            (
                "page:graph",
                "History",
                Page::Graph,
                Icon::Graph,
                "",
                "The commit graph, branches and the working tree",
                "graph log commits",
                false,
            ),
            (
                "page:review",
                "Review",
                Page::Review,
                Icon::PullRequest,
                "Ctrl+Shift+R",
                "Pull requests, reviews and issues",
                "pull requests prs merge requests",
                true,
            ),
            (
                "page:actions",
                "Actions",
                Page::GitHub,
                Icon::Play,
                "Ctrl+Shift+G",
                "Workflow runs, workflows, artifacts, caches, validation",
                "ci pipelines workflows runs",
                true,
            ),
            (
                "page:ship",
                "Ship",
                Page::Ship,
                Icon::Rocket,
                "Ctrl+Shift+D",
                "Releases, deployments, DORA metrics, packages, Kubernetes, Terraform",
                "release deploy",
                false,
            ),
            (
                "page:exposure",
                "Exposure",
                Page::Security,
                Icon::Shield,
                "Ctrl+Shift+S",
                "Secrets, sensitive files, OSINT and leaky history",
                "security secrets leaks",
                false,
            ),
            (
                "page:insights",
                "Insights",
                Page::Insights,
                Icon::Chart,
                "Ctrl+Shift+I",
                "Health, hotspots, ownership, branches, releases, debt",
                "analytics statistics",
                false,
            ),
            (
                "page:govern",
                "Govern",
                Page::Govern,
                Icon::Lock,
                "Ctrl+Shift+A",
                "Alerts, SBOM, images, protection, CODEOWNERS, secrets and variables",
                "admin governance",
                false,
            ),
            (
                "page:tools",
                "Tools",
                Page::Workbench,
                Icon::Terminal,
                "Ctrl+Shift+T",
                "Tasks, interactive rebase, bisect, file history, worktrees",
                "workbench",
                false,
            ),
            (
                "page:compare",
                "Compare",
                Page::Compare,
                Icon::Merge,
                "",
                "Compare two commits, branches or tags",
                "diff",
                false,
            ),
            (
                "page:settings",
                "Settings",
                Page::Settings,
                Icon::Gear,
                "Ctrl+,",
                "Appearance, behaviour, safety, git config, accounts",
                "preferences options",
                false,
            ),
        ];
        for (id, title, page, icon, key, hint, kw, needs_remote) in pages {
            if needs_remote && !remote {
                continue;
            }
            c.push(
                Command::new(id, title, GO, icon)
                    .hint(hint)
                    .key(key)
                    .kw(&format!("page open show {kw}"))
                    .run(vec![show(page)]),
            );
        }
        if github {
            c.push(
                Command::new("page:issues", "Issues", GO, Icon::Issue)
                    .hint("Issues of this repository on the Review page")
                    .kw("page bugs tickets")
                    .run(vec![
                        show(Page::Review),
                        review::Act::SetMode(review::Mode::Issues).into(),
                    ]),
            );
            for t in ci::Tab::ALL.into_iter().filter(|t| *t != ci::Tab::Runs) {
                c.push(
                    Command::new(
                        &format!("tab:actions:{}", t.label()),
                        format!("Actions › {}", t.label()),
                        GO,
                        Icon::Play,
                    )
                    .hint(ci_hint(t))
                    .kw("tab actions ci")
                    .run(vec![show(Page::GitHub), ci::Act::SetTab(t).into()]),
                );
            }
        }
        for t in SHIP_TABS {
            c.push(
                Command::new(
                    &format!("tab:ship:{}", t.label()),
                    format!("Ship › {}", t.label()),
                    GO,
                    Icon::Rocket,
                )
                .hint(t.hint())
                .kw("tab ship")
                .run(vec![
                    Action::DevOps(DevOpsAction::SetShipTab(t)),
                    show(Page::Ship),
                ]),
            );
        }
        for t in SecurityTab::ALL {
            c.push(
                Command::new(
                    &format!("tab:exposure:{}", t.label()),
                    format!("Exposure › {}", t.label()),
                    GO,
                    Icon::Shield,
                )
                .hint(t.hint())
                .kw("tab exposure security osint")
                .run(vec![show(Page::Security), Action::SetSecurityTab(t)]),
            );
        }
        for t in InsightsTab::ALL {
            c.push(
                Command::new(
                    &format!("tab:insights:{}", t.label()),
                    format!("Insights › {}", t.label()),
                    GO,
                    Icon::Chart,
                )
                .hint(t.hint())
                .kw("tab insights")
                .run(vec![show(Page::Insights), Action::SetInsightsTab(t)]),
            );
        }
        for t in govern::Tab::ALL {
            c.push(
                Command::new(
                    &format!("tab:govern:{}", t.label()),
                    format!("Govern › {}", t.label()),
                    GO,
                    Icon::Lock,
                )
                .hint(t.hint())
                .kw("tab govern admin")
                .run(vec![govern::Act::SetTab(t).into(), show(Page::Govern)]),
            );
        }
        for t in TOOLS_TABS {
            c.push(
                Command::new(
                    &format!("tab:tools:{}", t.label()),
                    format!("Tools › {}", t.label()),
                    GO,
                    Icon::Terminal,
                )
                .hint(t.hint())
                .kw("tab tools workbench")
                .run(vec![
                    show(Page::Workbench),
                    workbench::Act::SetTab(t).into(),
                ]),
            );
        }
        for s in SECTIONS {
            c.push(
                Command::new(
                    &format!("settings:{}", s.label()),
                    format!("Settings › {}", s.label()),
                    GO,
                    Icon::Gear,
                )
                .hint(s.hint())
                .kw("settings preferences options")
                .run(vec![
                    Action::DevOps(DevOpsAction::SetSettingsSection(s)),
                    show(Page::Settings),
                ]),
            );
        }
    }

    // ----- App -----
    let (theme_title, theme_hint) = if s.dark {
        ("Switch to the light theme", "The theme is dark now")
    } else {
        ("Switch to the dark theme", "The theme is light now")
    };
    c.push(
        Command::new("app:theme", theme_title, APP, Icon::Eye)
            .hint(theme_hint)
            .kw("toggle theme dark light appearance colours")
            .run(vec![Action::ToggleTheme]),
    );
    c.push(
        Command::new(
            "app:terminal",
            if s.show_terminal {
                "Hide the terminal"
            } else {
                "Show the terminal"
            },
            APP,
            Icon::Terminal,
        )
        .hint("The terminal panel at the bottom of the window")
        .key("Ctrl+`")
        .kw("toggle shell console panel")
        .run(vec![Action::ToggleTerminal]),
    );
    c.push(
        Command::new(
            "app:activity",
            if s.show_activity {
                "Hide the activity log"
            } else {
                "Show the activity log"
            },
            APP,
            Icon::List,
        )
        .hint("Every operation, read and API call with its commands")
        .key("Ctrl+L")
        .kw("toggle log history operations")
        .run(vec![Action::ToggleActivity]),
    );
    if s.safe_mode {
        c.push(
            Command::new("app:safe-off", "Turn safe mode off", APP, Icon::Shield)
                .hint("Safe mode is on: every change outside this machine is refused")
                .kw("safety disable allow writes")
                .run(vec![Action::SetSafeMode(false)]),
        );
    } else {
        c.push(
            Command::new("app:safe-on", "Turn safe mode on", APP, Icon::Shield)
                .hint("Refuse every change outside this machine: pushes, GitHub writes, clusters")
                .kw("safety enable block writes read only")
                .run(vec![Action::SetSafeMode(true)]),
        );
    }
    c.push(
        Command::new(
            "app:verbose",
            if s.verbose {
                "Log operations only"
            } else {
                "Log reads and API calls too"
            },
            APP,
            Icon::List,
        )
        .hint(if s.verbose {
            "The activity log records reads and API calls now"
        } else {
            "The activity log records operations and failures now"
        })
        .kw("verbose activity log toggle")
        .run(vec![Action::ToggleVerbose]),
    );
    c.push(
        Command::new(
            "app:status-bar",
            if s.show_status_bar {
                "Hide the status bar"
            } else {
                "Show the status bar"
            },
            APP,
            Icon::Info,
        )
        .kw("toggle bottom bar")
        .run(vec![Action::ToggleStatusBar]),
    );
    c.push(
        Command::new(
            "app:dates",
            if s.relative_dates {
                "Show dates as dates"
            } else {
                "Show dates as \"3 hours ago\""
            },
            APP,
            Icon::Clock,
        )
        .kw("relative absolute time toggle")
        .run(vec![Action::ToggleRelativeDates]),
    );
    c.push(
        Command::new(
            "app:author",
            if s.show_author {
                "Hide the author column"
            } else {
                "Show the author column"
            },
            APP,
            Icon::Graph,
        )
        .kw("graph toggle names")
        .run(vec![Action::ToggleAuthor]),
    );
    c.push(
        Command::new("app:open", "Open a repository…", APP, Icon::Folder)
            .hint("Pick a folder with a git repository")
            .key("Ctrl+O")
            .kw("add folder tab")
            .run(vec![Action::PickRepo]),
    );
    c.push(
        Command::new("app:clone", "Clone a repository…", APP, Icon::Download)
            .hint("Clone from a URL into a new folder")
            .key("Ctrl+Shift+O")
            .kw("git clone url download")
            .run(vec![Action::CloneDialog]),
    );
    if let Some(r) = repo {
        c.push(
            Command::new("app:reload", "Reload the repository", APP, Icon::Rerun)
                .hint("Read refs, history and the working tree again")
                .key("F5")
                .kw("refresh")
                .run(vec![Action::Reload(true)]),
        );
        c.push(
            Command::new("app:close", format!("Close {}", r.name), APP, Icon::Close)
                .hint("Close this repository's tab")
                .key("Ctrl+W")
                .kw("close tab")
                .run(vec![Action::CloseRepo(cx.active)]),
        );
        c.push(
            Command::new("app:find", "Find a commit in the graph", APP, Icon::Search)
                .hint("Search summaries, authors, SHAs and ref names in the graph")
                .key("Ctrl+F")
                .kw("search filter")
                .run(vec![show(Page::Graph), Action::FocusSearch]),
        );
    }
    let mut help = Command::new("palette:help", "Palette prefixes", PALETTE, Icon::Info)
        .hint("What > @ # / : ! ~ search")
        .kw("help keys shortcuts")
        .run(vec![super::Act::Query("?".into()).into()]);
    help.keep_open = true;
    c.push(help);
    if !s.ext.palette.recent.is_empty() {
        let mut clear = Command::new(
            "palette:clear",
            "Clear the palette's recent items",
            PALETTE,
            Icon::Trash,
        )
        .hint("Forget what was used from the palette (the Recent group and ranking)")
        .kw("history frecency reset")
        .run(vec![super::Act::ClearRecent.into()]);
        clear.keep_open = true;
        c.push(clear);
    }

    // ----- Git (active repository) -----
    if let Some(snap) = cx.snap() {
        let r = repo.expect("snapshot implies repository");
        let has_remotes = !snap.remotes.is_empty();
        let current = snap.current_branch();
        let detached = matches!(snap.head, hadion_core::Head::Detached(_));
        if has_remotes {
            c.push(
                Command::new("git:fetch", "Fetch", GIT, Icon::Fetch)
                    .hint("Fetch all remotes and prune")
                    .kw("download update remote")
                    .run(vec![Action::Run(GitOp::Fetch)]),
            );
        }
        if has_remotes && let Some(b) = current {
            let behind = match b.behind {
                0 => String::new(),
                n => format!(
                    " · {n} behind {}",
                    b.upstream.as_deref().unwrap_or("upstream")
                ),
            };
            for pp in [PullPref::Merge, PullPref::FastForwardOnly, PullPref::Rebase] {
                let default = pp == s.pull;
                let id = match pp {
                    PullPref::Merge => "git:pull-merge",
                    PullPref::FastForwardOnly => "git:pull-ff",
                    PullPref::Rebase => "git:pull-rebase",
                };
                c.push(
                    Command::new(
                        if default { "git:pull" } else { id },
                        pp.label(),
                        GIT,
                        Icon::Pull,
                    )
                    .hint(&format!(
                        "{}{behind}",
                        if default {
                            "The default pull"
                        } else {
                            "Once, without changing the default"
                        }
                    ))
                    .kw("update download merge rebase")
                    .run(vec![Action::Run(GitOp::Pull { mode: pp.mode() })]),
                );
            }
        }
        if has_remotes
            && !detached
            && let Some(b) = current
        {
            let remotes: Vec<String> = snap.remotes.iter().map(|r| r.name.clone()).collect();
            let upstream = b
                .upstream
                .as_deref()
                .filter(|_| !b.upstream_gone)
                .and_then(|u| crate::views::menus::split_remote(snap, u));
            let remote = upstream
                .as_ref()
                .and_then(|(r, _)| remotes.iter().position(|x| x == r))
                .unwrap_or(0);
            let hint = match (&b.upstream, b.ahead) {
                (Some(u), 0) => format!("Nothing new for {u}"),
                (Some(u), n) => format!(
                    "{} ahead of {u}",
                    crate::format::plural(n, "commit", "commits")
                ),
                (None, _) => "No upstream yet: choose a remote".to_owned(),
            };
            c.push(
                Command::new("git:push", format!("Push {}…", b.name), GIT, Icon::Push)
                    .hint(&hint)
                    .kw("upload publish remote")
                    .run(vec![Action::Dialog(Dialog::Push {
                        branch: b.name.clone(),
                        remotes,
                        remote,
                        set_upstream: upstream.is_none(),
                        force: false,
                    })]),
            );
        }
        if !snap.status.is_clean() {
            c.push(
                Command::new("git:stash", "Stash the changes…", GIT, Icon::Stash)
                    .hint(&format!(
                        "{} in the working tree",
                        crate::format::plural(snap.status.change_count(), "change", "changes")
                    ))
                    .kw("save shelve wip")
                    .run(vec![Action::Dialog(Dialog::Stash {
                        message: String::new(),
                        include_untracked: true,
                    })]),
            );
            c.push(
                Command::new("git:wip", "Show the uncommitted changes", GIT, Icon::Edit)
                    .hint("Select the working tree in the graph")
                    .kw("wip working tree status commit")
                    .run(vec![
                        show(Page::Graph),
                        Action::Select(crate::state::Selection::Wip),
                    ]),
            );
        }
        if let Some(st) = snap.stashes.first() {
            c.push(
                Command::new("git:pop", "Pop the newest stash", GIT, Icon::Pop)
                    .hint(&st.message)
                    .kw("unstash apply restore")
                    .run(vec![Action::Run(GitOp::StashPop { index: 0 })]),
            );
        }
        if let Some(u) = &r.undo {
            c.push(
                Command::new("git:undo", u.label.clone(), GIT, Icon::Undo)
                    .hint(&u.detail)
                    .key("Ctrl+Z")
                    .kw("undo revert reflog")
                    .run(vec![Action::Undo]),
            );
        }
        if let Some(head) = snap.head.target() {
            let (start, label) = match current {
                Some(b) => (b.name.clone(), b.name.clone()),
                None => (head.to_string(), "HEAD".to_owned()),
            };
            c.push(
                Command::new("git:branch", "Create a branch at HEAD…", GIT, Icon::Branch)
                    .hint(&format!("Starts at {label}"))
                    .kw("new branch checkout -b")
                    .run(vec![Action::Dialog(Dialog::CreateBranch {
                        name: String::new(),
                        start: start.clone(),
                        start_label: label.clone(),
                        checkout: true,
                    })]),
            );
            c.push(
                Command::new("git:tag", "Create a tag at HEAD…", GIT, Icon::Tag)
                    .hint(&format!("Points at {label}"))
                    .kw("new tag release version")
                    .run(vec![Action::Dialog(Dialog::CreateTag {
                        name: String::new(),
                        target: start,
                        target_label: label,
                        message: String::new(),
                    })]),
            );
            c.push(
                Command::new("git:head", "Jump to HEAD", GIT, Icon::Graph)
                    .hint("Select the checked-out commit in the graph")
                    .kw("current commit")
                    .run(vec![show(Page::Graph), Action::JumpTo(head)]),
            );
        }
        if matches!(
            snap.pending,
            Pending::Merge | Pending::Rebase | Pending::CherryPick | Pending::Revert
        ) {
            let what = snap.pending.label().to_lowercase();
            c.push(
                Command::new(
                    "git:continue",
                    format!("Continue the {what}"),
                    GIT,
                    Icon::Play,
                )
                .hint("Once every conflict is resolved and staged")
                .kw("resolve conflicts")
                .run(vec![Action::Run(GitOp::Continue(snap.pending))]),
            );
            c.push(
                Command::new("git:abort", format!("Abort the {what}…"), GIT, Icon::Stop)
                    .hint("Go back to where it started")
                    .kw("cancel conflicts")
                    .run(vec![Action::Dialog(Dialog::Confirm {
                        title: format!("Abort the {what}?"),
                        body: format!(
                            "Throws away the {what} in progress and its conflict resolutions, and goes back to where it started."
                        ),
                        confirm: format!("Abort {what}"),
                        danger: true,
                        ops: vec![GitOp::Abort(snap.pending)],
                    })]),
            );
        }
    }

    // ----- Areas -----
    if github {
        c.push(
            Command::new("gh:refresh", "Refresh the GitHub data", GITHUB, Icon::Rerun)
                .hint("Workflow runs, pull requests and checks of this repository")
                .kw("reload update api")
                .run(vec![Action::RefreshGitHub]),
        );
        if cx.head_branch().is_some() {
            c.push(
                Command::new(
                    "review:new-pr",
                    "New pull request…",
                    "Review",
                    Icon::PullRequest,
                )
                .hint("From the current branch")
                .kw("create open pr")
                .run(vec![show(Page::Review), review::Act::NewPr.into()]),
            );
        }
        c.push(
            Command::new("review:new-issue", "New issue…", "Review", Icon::Issue)
                .hint("Open an issue on GitHub")
                .kw("create bug report")
                .run(vec![show(Page::Review), review::Act::NewIssue.into()]),
        );
        c.push(
            Command::new(
                "review:refresh",
                "Refresh the pull requests",
                "Review",
                Icon::Rerun,
            )
            .hint("Read the pull request list again")
            .kw("reload prs")
            .run(vec![show(Page::Review), review::Act::Refresh.into()]),
        );
        c.push(
            Command::new("ci:dispatch", "Run a workflow…", "Actions", Icon::Play)
                .hint("Pick a workflow with a manual trigger and its inputs")
                .kw("dispatch trigger workflow_dispatch start")
                .run(vec![
                    show(Page::GitHub),
                    ci::Act::SetTab(ci::Tab::Workflows).into(),
                ]),
        );
        c.push(
            Command::new(
                "ci:validate",
                "Validate the workflow files",
                "Actions",
                Icon::Check,
            )
            .hint("Lint .github/workflows locally: syntax, pinning, permissions")
            .kw("lint check yaml")
            .run(vec![
                show(Page::GitHub),
                ci::Act::SetTab(ci::Tab::Validate).into(),
                ci::Act::Validate.into(),
            ]),
        );
        c.push(
            Command::new("ship:new-release", "New release…", "Ship", Icon::Rocket)
                .hint("Tag, notes from the history, assets")
                .kw("publish version tag")
                .run(vec![
                    ship::Act::NewRelease.into(),
                    Action::DevOps(DevOpsAction::SetShipTab(ShipTab::Releases)),
                    show(Page::Ship),
                ]),
        );
        c.push(
            Command::new("ship:where-head", "Where is HEAD?", "Ship", Icon::Cloud)
                .hint("Branches, tags, releases and deployments that contain HEAD")
                .kw("deployed environment contains")
                .run(vec![
                    ship::Act::WhereHead.into(),
                    Action::DevOps(DevOpsAction::SetShipTab(ShipTab::WhereIsMyCommit)),
                    show(Page::Ship),
                ]),
        );
    }
    if has_repo {
        let tools = |t: workbench::Tab| -> Vec<Action> {
            vec![show(Page::Workbench), workbench::Act::SetTab(t).into()]
        };
        c.push(
            Command::new("tools:run-task", "Run a task…", "Tools", Icon::Play)
                .hint("npm scripts, make targets, cargo, just, Taskfile and more")
                .kw("script npm make build test")
                .run(tools(workbench::Tab::Tasks)),
        );
        let mut find = tools(workbench::Tab::Tasks);
        find.push(workbench::Act::DiscoverTasks.into());
        c.push(
            Command::new(
                "tools:find-tasks",
                "Find the tasks again",
                "Tools",
                Icon::Rerun,
            )
            .hint("Read package.json, Makefile, Cargo.toml and the others again")
            .kw("discover rescan")
            .run(find),
        );
        c.push(
            Command::new("tools:bisect", "Start bisect…", "Tools", Icon::Bug)
                .hint("Find the commit that broke something")
                .kw("bisect bad good regression")
                .run(tools(workbench::Tab::Bisect)),
        );
        c.push(
            Command::new("tools:rebase", "Interactive rebase…", "Tools", Icon::Edit)
                .hint("Reorder, reword, squash, fix up or drop commits")
                .kw("rebase -i squash fixup reword")
                .run(tools(workbench::Tab::Rebase)),
        );
        c.push(
            Command::new(
                "exposure:scan",
                "Scan for secrets",
                "Exposure",
                Icon::Shield,
            )
            .hint("The working tree, staged changes and HEAD")
            .kw("security credentials keys leaks")
            .run(vec![
                show(Page::Security),
                Action::ScanSecrets { history: false },
            ]),
        );
        c.push(
            Command::new(
                "exposure:scan-history",
                "Scan for secrets, history included",
                "Exposure",
                Icon::Shield,
            )
            .hint("Also every commit of the history depth set on the Exposure page")
            .kw("security credentials keys leaks commits")
            .run(vec![
                show(Page::Security),
                Action::ScanSecrets { history: true },
            ]),
        );
        c.push(
            Command::new(
                "insights:run",
                "Run the insights analysis",
                "Insights",
                Icon::Chart,
            )
            .hint("Hotspots, ownership, branches, releases and debt from the history")
            .kw("analyse analyze statistics")
            .run(vec![show(Page::Insights), Action::RunInsights]),
        );
        c.push(
            Command::new(
                "home:fetch-all",
                "Fetch every open repository…",
                "Home",
                Icon::Fetch,
            )
            .hint(&format!(
                "{} open; asks first",
                crate::format::plural(cx.repos.len(), "repository", "repositories")
            ))
            .kw("fetch all update")
            .run(vec![home::Act::FetchAll.into()]),
        );
        c.push(
            Command::new("home:refresh", "Refresh the inbox", "Home", Icon::Inbox)
                .hint("Review requests, your pull requests, mentions, assigned issues")
                .kw("notifications reload")
                .run(vec![show(Page::Home), home::Act::Refresh.into()]),
        );
    }
    c
}

/// A page command: id, title, page, icon, shortcut, hint, keywords, and
/// whether it needs a GitHub / GitLab / Azure DevOps remote.
type PageRow = (
    &'static str,
    &'static str,
    Page,
    Icon,
    &'static str,
    &'static str,
    &'static str,
    bool,
);

const SHIP_TABS: [ShipTab; 7] = [
    ShipTab::Releases,
    ShipTab::Environments,
    ShipTab::WhereIsMyCommit,
    ShipTab::Dora,
    ShipTab::Packages,
    ShipTab::Kubernetes,
    ShipTab::Terraform,
];

const TOOLS_TABS: [workbench::Tab; 11] = [
    workbench::Tab::Tasks,
    workbench::Tab::Console,
    workbench::Tab::Rebase,
    workbench::Tab::Bisect,
    workbench::Tab::History,
    workbench::Tab::Worktrees,
    workbench::Tab::Stashes,
    workbench::Tab::Submodules,
    workbench::Tab::Lfs,
    workbench::Tab::Patches,
    workbench::Tab::Hooks,
];

const SECTIONS: [Section; 14] = [
    Section::Appearance,
    Section::Behaviour,
    Section::Safety,
    Section::Terminal,
    Section::GitConfig,
    Section::Identities,
    Section::Keys,
    Section::GitHubAccount,
    Section::Ai,
    Section::Notifications,
    Section::Integrations,
    Section::Updates,
    Section::CrashReports,
    Section::About,
];

fn ci_hint(t: ci::Tab) -> &'static str {
    match t {
        ci::Tab::Runs => "Workflow runs, jobs, steps and logs",
        ci::Tab::Workflows => "Workflows, their history, flaky jobs, manual runs",
        ci::Tab::Artifacts => "Artifacts of the runs: size, expiry, download",
        ci::Tab::Caches => "Actions caches by key and branch",
        ci::Tab::Validate => "Lint the workflow files locally",
        ci::Tab::Local => "Run workflows on this machine with act",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{RepoState, Settings};

    fn ids(c: &Catalog) -> Vec<&str> {
        c.cmds.iter().map(|c| c.entry.id.as_str()).collect()
    }

    #[test]
    fn without_a_repository_only_app_commands() {
        let settings = Settings::default();
        let cx = Cx {
            settings: &settings,
            repos: &[],
            active: 0,
        };
        let c = catalog(&cx);
        let ids = ids(&c);
        assert!(ids.contains(&"app:theme"));
        assert!(ids.contains(&"app:open"));
        assert!(ids.contains(&"app:clone"));
        assert!(
            !ids.iter()
                .any(|i| i.starts_with("page:") || i.starts_with("git:"))
        );
        // Unique ids, everything runs something.
        let mut seen = std::collections::HashSet::new();
        for cmd in &c.cmds {
            assert!(seen.insert(cmd.entry.id.clone()), "{}", cmd.entry.id);
            assert!(!cmd.actions.is_empty());
            assert!(!cmd.entry.title.is_empty());
        }
    }

    #[test]
    fn toggles_follow_the_settings() {
        let mut settings = Settings::default();
        let repos = [RepoState::new(1, "C:/x".into())];
        settings.dark = true;
        settings.safe_mode = false;
        let cx = Cx {
            settings: &settings,
            repos: &repos,
            active: 0,
        };
        let c = catalog(&cx);
        assert_eq!(
            c.get("app:theme").unwrap().entry.title,
            "Switch to the light theme"
        );
        assert!(c.get("app:safe-on").is_some());
        assert!(c.get("app:safe-off").is_none());
        // No remote: no Review or Actions page, no GitHub actions.
        assert!(c.get("page:review").is_none());
        assert!(c.get("gh:refresh").is_none());
        assert!(c.get("page:tools").is_some());
        assert!(c.get("tab:tools:Bisect").is_some());
        let before = stamp(&cx);
        settings.safe_mode = true;
        settings.dark = false;
        let cx = Cx {
            settings: &settings,
            repos: &repos,
            active: 0,
        };
        assert_ne!(stamp(&cx), before);
        let c = catalog(&cx);
        assert!(c.get("app:safe-off").is_some());
        assert_eq!(
            c.get("app:theme").unwrap().entry.title,
            "Switch to the dark theme"
        );
    }

    #[test]
    fn github_remote_adds_area_commands() {
        let settings = Settings::default();
        let mut r = RepoState::new(1, "C:/x".into());
        r.github_remote = Some(("o".into(), "r".into()));
        let repos = [r];
        let cx = Cx {
            settings: &settings,
            repos: &repos,
            active: 0,
        };
        let c = catalog(&cx);
        for id in [
            "page:review",
            "page:actions",
            "page:issues",
            "gh:refresh",
            "ci:validate",
            "ship:new-release",
            "tab:actions:Workflows",
        ] {
            assert!(c.get(id).is_some(), "{id}");
        }
        // The Runs tab is the Actions page itself.
        assert!(c.get("tab:actions:Runs").is_none());
    }
}
