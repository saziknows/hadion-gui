//! What can be done with an item ([`Verb`]), in which order (the first is
//! what Enter does), how each is labelled, and the app actions it runs.
//! Changes and network operations keep the confirmations they have
//! elsewhere in the app (merge and rebase ask first here too, since a
//! palette row is one key away).

use std::path::{Path, PathBuf};

use gitvis_core::devops::palette::{Item, Kind, Target};
use gitvis_core::ops::{GitOp, MergeMode};
use gitvis_core::{Oid, RefKind};

use super::Cx;
use super::commands::Catalog;
use crate::devops::{review, ship, workbench};
use crate::state::{Action, Dialog, Page, ToastKind};
use crate::views::menus;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Verb {
    /// A command; a task.
    Run,
    ShowTask,
    Checkout,
    Jump,
    Merge,
    Rebase,
    CopyName,
    /// Ship › Where is my commit.
    WhereIs,
    History,
    OpenFile,
    ShowInFolder,
    CopyPath,
    CopySha,
    /// Check out a commit (detached HEAD).
    CheckoutCommit,
    OpenReview,
    OpenBrowser,
    CheckoutPr,
    CopyLink,
    OpenRun,
    Switch,
    OpenRepo,
    OpenFolder,
    UsePrefix,
    /// Remove from the Recent group.
    Forget,
}

impl Verb {
    pub const ALL: [Verb; 24] = [
        Verb::Run,
        Verb::ShowTask,
        Verb::Checkout,
        Verb::Jump,
        Verb::Merge,
        Verb::Rebase,
        Verb::CopyName,
        Verb::WhereIs,
        Verb::History,
        Verb::OpenFile,
        Verb::ShowInFolder,
        Verb::CopyPath,
        Verb::CopySha,
        Verb::CheckoutCommit,
        Verb::OpenReview,
        Verb::OpenBrowser,
        Verb::CheckoutPr,
        Verb::CopyLink,
        Verb::OpenRun,
        Verb::Switch,
        Verb::OpenRepo,
        Verb::OpenFolder,
        Verb::UsePrefix,
        Verb::Forget,
    ];

    /// Stable id for frecency keys.
    pub fn id(self) -> &'static str {
        match self {
            Verb::Run => "run",
            Verb::ShowTask => "show-task",
            Verb::Checkout => "checkout",
            Verb::Jump => "jump",
            Verb::Merge => "merge",
            Verb::Rebase => "rebase",
            Verb::CopyName => "copy-name",
            Verb::WhereIs => "where",
            Verb::History => "history",
            Verb::OpenFile => "open",
            Verb::ShowInFolder => "show-in-folder",
            Verb::CopyPath => "copy-path",
            Verb::CopySha => "copy-sha",
            Verb::CheckoutCommit => "checkout-commit",
            Verb::OpenReview => "review",
            Verb::OpenBrowser => "browser",
            Verb::CheckoutPr => "checkout-pr",
            Verb::CopyLink => "copy-link",
            Verb::OpenRun => "open-run",
            Verb::Switch => "switch",
            Verb::OpenRepo => "open-repo",
            Verb::OpenFolder => "open-folder",
            Verb::UsePrefix => "prefix",
            Verb::Forget => "forget",
        }
    }

    pub fn from_id(s: &str) -> Option<Verb> {
        Verb::ALL.into_iter().find(|v| v.id() == s)
    }

    /// A word or two for the key hints (`Enter  Check out`).
    pub fn short(self) -> &'static str {
        match self {
            Verb::Run => "Run",
            Verb::ShowTask => "Show",
            Verb::Checkout => "Check out",
            Verb::Jump => "Jump to",
            Verb::Merge => "Merge",
            Verb::Rebase => "Rebase",
            Verb::CopyName => "Copy name",
            Verb::WhereIs => "Where is it",
            Verb::History => "File history",
            Verb::OpenFile => "Open",
            Verb::ShowInFolder => "Show in folder",
            Verb::CopyPath => "Copy path",
            Verb::CopySha => "Copy SHA",
            Verb::CheckoutCommit => "Check out",
            Verb::OpenReview => "Open",
            Verb::OpenBrowser => "Open on GitHub",
            Verb::CheckoutPr => "Check out",
            Verb::CopyLink => "Copy link",
            Verb::OpenRun => "Open",
            Verb::Switch => "Switch",
            Verb::OpenRepo => "Open",
            Verb::OpenFolder => "Open folder",
            Verb::UsePrefix => "Search",
            Verb::Forget => "Remove",
        }
    }

    /// Opens a confirmation (or a dialog) before anything changes.
    pub fn asks_first(self, item: &Item) -> bool {
        match self {
            Verb::Merge | Verb::Rebase | Verb::CheckoutCommit | Verb::CheckoutPr => true,
            Verb::Checkout => item.kind() == Kind::Tag,
            _ => false,
        }
    }
}

/// What can be done with `item`, the default first.
pub fn verbs(item: &Item, cx: &Cx, catalog: &Catalog) -> Vec<Verb> {
    let github = cx.github().is_some();
    match &item.target {
        Target::Command(id) => {
            if catalog.get(id).is_some() {
                vec![Verb::Run]
            } else {
                Vec::new()
            }
        }
        Target::Task(_) => vec![Verb::Run, Verb::ShowTask],
        Target::Ref { name, kind, .. } => {
            let head = cx.head_branch();
            let is_head = *kind == RefKind::Local && head == Some(name.as_str());
            let mut v = Vec::new();
            match kind {
                RefKind::Tag => v.extend([Verb::Jump, Verb::Checkout]),
                _ if is_head => v.push(Verb::Jump),
                _ => v.extend([Verb::Checkout, Verb::Jump]),
            }
            if let Some(h) = head
                && h != name
                && !(*kind == RefKind::Local && is_head)
            {
                v.extend([Verb::Merge, Verb::Rebase]);
            }
            v.push(Verb::CopyName);
            v
        }
        Target::File(path) => {
            let open = if runs_when_opened(path) {
                Verb::ShowInFolder
            } else {
                Verb::OpenFile
            };
            vec![Verb::History, open, Verb::CopyPath]
        }
        Target::Commit(_) => {
            let mut v = vec![Verb::Jump, Verb::CopySha, Verb::CheckoutCommit];
            if github {
                v.push(Verb::WhereIs);
            }
            v
        }
        Target::Pr(_) => {
            let mut v = vec![Verb::OpenReview];
            if github {
                v.extend([Verb::OpenBrowser, Verb::CheckoutPr, Verb::CopyLink]);
            }
            v
        }
        Target::Issue(_) => {
            let mut v = vec![Verb::OpenReview];
            if github {
                v.extend([Verb::OpenBrowser, Verb::CopyLink]);
            }
            v
        }
        Target::Run(_) => {
            let mut v = vec![Verb::OpenRun];
            if github {
                v.extend([Verb::OpenBrowser, Verb::CopyLink]);
            }
            v
        }
        Target::Repo(path) => {
            let open = cx.repos.iter().position(|r| same_path(&r.workdir, path));
            match open {
                Some(_) => vec![Verb::Switch, Verb::OpenFolder, Verb::CopyPath],
                None => vec![Verb::OpenRepo, Verb::OpenFolder, Verb::CopyPath],
            }
        }
        Target::Prefix(_) => vec![Verb::UsePrefix],
    }
}

/// The default action (Enter, click).
pub fn default_verb(item: &Item, cx: &Cx, catalog: &Catalog) -> Option<Verb> {
    verbs(item, cx, catalog).into_iter().next()
}

/// A full sentence for the actions list.
pub fn label(verb: Verb, item: &Item, cx: &Cx, catalog: &Catalog) -> String {
    let name = &item.title;
    let head = cx.head_branch().unwrap_or("HEAD");
    match verb {
        Verb::Run => match &item.target {
            Target::Command(id) => catalog
                .get(id)
                .map(|c| c.entry.title.clone())
                .unwrap_or_else(|| name.clone()),
            _ => format!("Run {name}"),
        },
        Verb::ShowTask => format!("Show {name} on the Tools page"),
        Verb::Checkout => match item.kind() {
            Kind::Tag => format!("Check out {name} (detached HEAD)"),
            Kind::Remote => match cx.snap().and_then(|s| s.remote_branch(name)) {
                Some(rb) if cx.snap().and_then(|s| s.branch(&rb.branch)).is_some() => {
                    format!("Check out {} (the local branch)", rb.branch)
                }
                Some(rb) => format!("Check out as a new local branch {}", rb.branch),
                None => format!("Check out {name}"),
            },
            _ => format!("Check out {name}"),
        },
        Verb::Jump => match item.kind() {
            Kind::Commit => "Jump to the commit in the graph".to_owned(),
            _ => format!("Jump to {name} in the graph"),
        },
        Verb::Merge => format!("Merge {name} into {head}"),
        Verb::Rebase => format!("Rebase {head} onto {name}"),
        Verb::CopyName => format!("Copy \"{name}\""),
        Verb::WhereIs => "Where is this commit? Branches, tags, releases, deployments".to_owned(),
        Verb::History => "Show the file's history".to_owned(),
        Verb::OpenFile => "Open the file".to_owned(),
        Verb::ShowInFolder => "Show the file in its folder (it would run if opened)".to_owned(),
        Verb::CopyPath => "Copy the path".to_owned(),
        Verb::CopySha => "Copy the SHA".to_owned(),
        Verb::CheckoutCommit => "Check out this commit (detached HEAD)".to_owned(),
        Verb::OpenReview => "Open on the Review page".to_owned(),
        Verb::OpenBrowser => "Open on GitHub".to_owned(),
        Verb::CheckoutPr => "Check out the pull request's branch".to_owned(),
        Verb::CopyLink => "Copy the link".to_owned(),
        Verb::OpenRun => "Open the run on the Actions page".to_owned(),
        Verb::Switch => format!("Switch to {name}"),
        Verb::OpenRepo => format!("Open {name} in a new tab"),
        Verb::OpenFolder => "Open the folder".to_owned(),
        Verb::UsePrefix => format!("Search {}", name.to_lowercase()),
        Verb::Forget => "Remove from recent".to_owned(),
    }
}

/// The app actions of `verb` on `item`. Empty when it cannot run here
/// (the reason comes back as a toast).
pub fn actions(verb: Verb, item: &Item, cx: &Cx, catalog: &Catalog) -> Vec<Action> {
    let snap = cx.snap();
    let unavailable = |why: &str| vec![Action::Toast(ToastKind::Info, why.to_owned())];
    match (&item.target, verb) {
        (Target::Command(id), Verb::Run) => match catalog.get(id) {
            Some(c) => c.actions.clone(),
            None => unavailable(&format!("{} is not available here", item.title)),
        },
        (Target::Task(id), Verb::Run) => vec![
            Action::ShowPage(Page::Workbench),
            workbench::Act::SetTab(workbench::Tab::Tasks).into(),
            workbench::Act::SelectTask(Some(id.clone())).into(),
            workbench::Act::RunTask(id.clone()).into(),
        ],
        (Target::Task(id), Verb::ShowTask) => vec![
            Action::ShowPage(Page::Workbench),
            workbench::Act::SetTab(workbench::Tab::Tasks).into(),
            workbench::Act::SelectTask(Some(id.clone())).into(),
        ],
        (Target::Ref { name, kind, .. }, Verb::Checkout) => {
            let Some(snap) = snap else {
                return unavailable("The repository is still loading");
            };
            match kind {
                RefKind::Local => vec![Action::Run(GitOp::Switch {
                    branch: name.clone(),
                })],
                RefKind::Remote => match snap.remote_branch(name) {
                    Some(rb) => vec![menus::checkout_remote(snap, rb)],
                    None => unavailable(&format!("{name} is gone")),
                },
                RefKind::Tag => vec![Action::Dialog(menus::detach_dialog(name))],
            }
        }
        (Target::Ref { name, kind, .. }, Verb::Jump) => {
            let target = snap.and_then(|s| match kind {
                RefKind::Local => s.branch(name).map(|b| b.target),
                RefKind::Remote => s.remote_branch(name).map(|b| b.target),
                RefKind::Tag => s.tags.iter().find(|t| t.name == *name).map(|t| t.target),
            });
            match target {
                Some(oid) => vec![Action::ShowPage(Page::Graph), Action::JumpTo(oid)],
                None => unavailable(&format!("{name} is gone")),
            }
        }
        (Target::Ref { name, .. }, Verb::Merge) => {
            let Some(head) = cx.head_branch() else {
                return unavailable("HEAD is not on a branch");
            };
            let op = GitOp::Merge {
                from: name.clone(),
                mode: MergeMode::Default,
            };
            vec![confirm(
                format!("Merge {name} into {head}?"),
                format!(
                    "Fast-forwards {head} when it can, otherwise creates a merge commit. Conflicts stop the merge for you to resolve. Undo with Ctrl+Z (reflog)."
                ),
                "Merge",
                false,
                op,
            )]
        }
        (Target::Ref { name, .. }, Verb::Rebase) => {
            let Some(head) = cx.head_branch() else {
                return unavailable("HEAD is not on a branch");
            };
            let op = GitOp::Rebase {
                onto: name.clone(),
                branch: None,
            };
            vec![confirm(
                format!("Rebase {head} onto {name}?"),
                format!(
                    "Replays the commits of {head} that {name} does not have on top of {name}. This rewrites them: if {head} was pushed, the next push needs force. Undo with Ctrl+Z (reflog)."
                ),
                "Rebase",
                false,
                op,
            )]
        }
        (Target::Ref { name, .. }, Verb::CopyName) => vec![Action::Copy(name.clone())],
        (Target::File(path), Verb::History) => vec![
            Action::ShowPage(Page::Workbench),
            workbench::Act::SetTab(workbench::Tab::History).into(),
            workbench::Act::FileHistory(path.clone()).into(),
        ],
        (Target::File(path), Verb::OpenFile) => match cx.repo() {
            Some(r) => vec![Action::OpenPath(r.workdir.join(path))],
            None => unavailable("No repository is open"),
        },
        (Target::File(path), Verb::ShowInFolder) => match cx.repo() {
            Some(r) => {
                let full = r.workdir.join(path);
                let dir = full
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| r.workdir.clone());
                vec![Action::OpenPath(dir)]
            }
            None => unavailable("No repository is open"),
        },
        (Target::File(path), Verb::CopyPath) => vec![Action::Copy(path.clone())],
        (Target::Commit(sha), Verb::Jump) => match Oid::from_str(sha) {
            Ok(oid) => vec![Action::ShowPage(Page::Graph), Action::JumpTo(oid)],
            Err(_) => unavailable("Not a commit id"),
        },
        (Target::Commit(sha), Verb::CopySha) => vec![Action::Copy(sha.clone())],
        (Target::Commit(sha), Verb::CheckoutCommit) => {
            vec![Action::Dialog(menus::detach_dialog(sha))]
        }
        (Target::Commit(sha), Verb::WhereIs) => vec![
            ship::Act::WhereRev(sha.clone()).into(),
            Action::DevOps(crate::devops::DevOpsAction::SetShipTab(
                crate::devops::ShipTab::WhereIsMyCommit,
            )),
            Action::ShowPage(Page::Ship),
        ],
        (Target::Pr(n), Verb::OpenReview) => vec![Action::OpenPr(*n)],
        (Target::Issue(n), Verb::OpenReview) => vec![
            Action::ShowPage(Page::Review),
            review::Act::SetMode(review::Mode::Issues).into(),
            review::Act::OpenIssue(*n).into(),
        ],
        (Target::Pr(n), Verb::CheckoutPr) => vec![review::Act::Checkout(*n).into()],
        (Target::Run(id), Verb::OpenRun) => vec![
            Action::ShowPage(Page::GitHub),
            Action::OpenRun { run: *id, job: None },
        ],
        (t @ (Target::Pr(_) | Target::Issue(_) | Target::Run(_)), Verb::OpenBrowser) => {
            match web_url(t, cx) {
                Some(u) => vec![Action::OpenUrl(u)],
                None => unavailable("This repository has no GitHub remote"),
            }
        }
        (t @ (Target::Pr(_) | Target::Issue(_) | Target::Run(_)), Verb::CopyLink) => {
            match web_url(t, cx) {
                Some(u) => vec![Action::Copy(u)],
                None => unavailable("This repository has no GitHub remote"),
            }
        }
        (Target::Repo(path), Verb::Switch) => {
            match cx.repos.iter().position(|r| same_path(&r.workdir, path)) {
                Some(i) => vec![Action::Activate(i)],
                None => vec![Action::OpenRepo(path.clone())],
            }
        }
        (Target::Repo(path), Verb::OpenRepo) => vec![Action::OpenRepo(path.clone())],
        (Target::Repo(path), Verb::OpenFolder) => vec![Action::OpenPath(path.clone())],
        (Target::Repo(path), Verb::CopyPath) => {
            vec![Action::Copy(path.display().to_string())]
        }
        _ => unavailable("That does not apply here"),
    }
}

/// A confirmation with the exact command, then the operation.
fn confirm(title: String, body: String, button: &str, danger: bool, op: GitOp) -> Action {
    let details = op.commands().iter().map(|c| c.join(" ")).collect();
    Action::Dialog(Dialog::ConfirmAction {
        title,
        body,
        confirm: button.to_owned(),
        danger,
        details,
        action: Box::new(Action::Run(op)),
    })
}

/// The GitHub page of a pull request, issue or run.
pub fn web_url(t: &Target, cx: &Cx) -> Option<String> {
    let (owner, repo) = cx.github()?;
    let base = format!("https://github.com/{owner}/{repo}");
    Some(match t {
        Target::Pr(n) => format!("{base}/pull/{n}"),
        Target::Issue(n) => format!("{base}/issues/{n}"),
        Target::Run(id) => format!("{base}/actions/runs/{id}"),
        _ => return None,
    })
}

/// Paths are compared case-insensitively with either slash (Windows).
pub fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('\\', "/").to_lowercase();
    norm(a) == norm(b)
}

/// Opening the file with the system would run it (programs and scripts):
/// the palette shows it in its folder instead.
pub fn runs_when_opened(path: &str) -> bool {
    const RUNS: [&str; 34] = [
        "exe", "com", "bat", "cmd", "ps1", "psm1", "psd1", "vbs", "vbe", "js", "jse", "wsf", "wsh",
        "msi", "msp", "msc", "scr", "lnk", "reg", "cpl", "hta", "jar", "py", "pyw", "pl", "rb",
        "sh", "bash", "appref-ms", "application", "gadget", "inf", "scf", "url",
    ];
    let ext = PathBuf::from(path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    RUNS.contains(&ext.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{RepoState, Settings};
    use gitvis_core::devops::palette::sources;

    fn cx<'a>(settings: &'a Settings, repos: &'a [RepoState]) -> Cx<'a> {
        Cx {
            settings,
            repos,
            active: 0,
        }
    }

    #[test]
    fn ids_round_trip() {
        for v in Verb::ALL {
            assert_eq!(Verb::from_id(v.id()), Some(v));
            assert!(!v.short().is_empty());
        }
        assert_eq!(Verb::from_id("nope"), None);
    }

    #[test]
    fn executables_are_never_opened() {
        assert!(runs_when_opened("scripts/build.ps1"));
        assert!(runs_when_opened("tools/Setup.EXE"));
        assert!(runs_when_opened("a/b.cmd"));
        assert!(!runs_when_opened("src/main.rs"));
        assert!(!runs_when_opened("README"));
        let settings = Settings::default();
        let repos = [RepoState::new(1, "C:/x".into())];
        let catalog = Catalog::default();
        let file = sources::file_items(&["run.bat".into()]).remove(0);
        let v = verbs(&file, &cx(&settings, &repos), &catalog);
        assert_eq!(v, [Verb::History, Verb::ShowInFolder, Verb::CopyPath]);
        let a = actions(Verb::ShowInFolder, &file, &cx(&settings, &repos), &catalog);
        assert!(matches!(&a[..], [Action::OpenPath(p)] if p == Path::new("C:/x")));
    }

    #[test]
    fn file_history_goes_to_the_tools_page() {
        let settings = Settings::default();
        let repos = [RepoState::new(1, "C:/x".into())];
        let catalog = Catalog::default();
        let file = sources::file_items(&["src/a.rs".into()]).remove(0);
        let c = cx(&settings, &repos);
        assert_eq!(default_verb(&file, &c, &catalog), Some(Verb::History));
        let a = actions(Verb::History, &file, &c, &catalog);
        assert!(matches!(a[0], Action::ShowPage(Page::Workbench)));
        assert!(matches!(
            &a[2],
            Action::DevOps(crate::devops::DevOpsAction::Workbench(
                workbench::Act::FileHistory(p)
            )) if p == "src/a.rs"
        ));
        assert!(matches!(&actions(Verb::CopyPath, &file, &c, &catalog)[..], [Action::Copy(p)] if p == "src/a.rs"));
    }

    #[test]
    fn web_links_need_a_github_remote() {
        let settings = Settings::default();
        let mut r = RepoState::new(1, "C:/x".into());
        let pr = sources::pr_items(&[sources::PrEntry {
            number: 5,
            title: "T".into(),
            ..Default::default()
        }])
        .remove(0);
        let catalog = Catalog::default();
        {
            let repos = [RepoState::new(1, "C:/x".into())];
            let c = cx(&settings, &repos);
            assert_eq!(verbs(&pr, &c, &catalog), [Verb::OpenReview]);
            assert!(web_url(&Target::Pr(5), &c).is_none());
        }
        r.github_remote = Some(("o".into(), "r".into()));
        let repos = [r];
        let c = cx(&settings, &repos);
        assert_eq!(
            verbs(&pr, &c, &catalog),
            [
                Verb::OpenReview,
                Verb::OpenBrowser,
                Verb::CheckoutPr,
                Verb::CopyLink
            ]
        );
        assert_eq!(
            web_url(&Target::Run(9), &c).as_deref(),
            Some("https://github.com/o/r/actions/runs/9")
        );
        assert!(matches!(&actions(Verb::OpenReview, &pr, &c, &catalog)[..], [Action::OpenPr(5)]));
        // Checking out asks first (through the review area).
        assert!(Verb::CheckoutPr.asks_first(&pr));
    }

    #[test]
    fn repos_switch_or_open() {
        let settings = Settings::default();
        let repos = [RepoState::new(1, "C:/a".into())];
        let c = cx(&settings, &repos);
        let catalog = Catalog::default();
        let items = sources::repo_items(&[
            sources::RepoEntry {
                path: "c:\\a".into(),
                name: "a".into(),
                open: Some(0),
                active: true,
                branch: None,
            },
            sources::RepoEntry {
                path: "C:/b".into(),
                name: "b".into(),
                open: None,
                active: false,
                branch: None,
            },
        ]);
        assert_eq!(verbs(&items[0], &c, &catalog)[0], Verb::Switch);
        assert!(matches!(&actions(Verb::Switch, &items[0], &c, &catalog)[..], [Action::Activate(0)]));
        assert_eq!(verbs(&items[1], &c, &catalog)[0], Verb::OpenRepo);
        assert!(matches!(&actions(Verb::OpenRepo, &items[1], &c, &catalog)[..], [Action::OpenRepo(p)] if p == Path::new("C:/b")));
    }

    #[test]
    fn missing_commands_explain_themselves() {
        let settings = Settings::default();
        let c = cx(&settings, &[]);
        let catalog = Catalog::default();
        let item = sources::command_items(&[sources::CommandEntry {
            id: "gone".into(),
            title: "Gone".into(),
            ..Default::default()
        }])
        .remove(0);
        assert!(verbs(&item, &c, &catalog).is_empty());
        assert!(matches!(
            &actions(Verb::Run, &item, &c, &catalog)[..],
            [Action::Toast(ToastKind::Info, m)] if m.contains("not available")
        ));
    }
}
