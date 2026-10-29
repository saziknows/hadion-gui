//! Automation: desktop notifications (opt-in polling for finished runs on
//! your branches and new review requests), repository groups with batch
//! operations (Home page), Jira / Linear links (commit panel, pull
//! requests, branch names, the commit message), release helpers (version
//! bumps and CHANGELOG.md when publishing on the Ship page).
//!
//! - [`notifications`]: the poll timer, the diff, delivery, the Settings
//!   section and the status bar item with its list.
//! - [`integrations`]: issue-tracker settings, key chips in the hooks, title
//!   lookups (credentials in the keychain, sent on curl's standard input).
//! - [`groups`]: the Home page's Groups section and the batch operations.
//! - [`release`]: the "Prepare the release commit" section of the publish form.
//! - [`shared`]: what the Home / Ship views and the hooks read.

mod groups;
mod integrations;
mod notifications;
mod release;
mod shared;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use hadion_core::Commit;
use hadion_core::devops::automation::groups::{Batch, Group, RepoResult, RepoStatus};
use hadion_core::devops::automation::links::{LinkPrefs, Tracker};
use hadion_core::devops::automation::lookup::IssueInfo;
use hadion_core::devops::automation::notify::RunWatch;
use hadion_core::devops::automation::release::Discovered;

use crate::devops::ctx::Ctx;
use crate::devops::global::{GlobalCx, GlobalView};
use crate::devops::hooks::HookCx;
use crate::devops::settings_page::Section;
use crate::state::{Action, Page, RepoState, Settings};

pub use groups::page as groups_page;
pub use release::section as release_section;

/// A secret typed by the user on its way to the keychain: never printed by
/// `Debug` (actions are cloned and may be logged).
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: String) -> Self {
        Self(s)
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(•••)")
    }
}

#[derive(Default)]
pub struct State {
    pub notify: notifications::Notify,
    pub links: integrations::LinksUi,
}

/// What the release section asks to prepare.
#[derive(Clone, Debug)]
pub struct PrepareReq {
    pub dir: PathBuf,
    pub tag: String,
    pub version: String,
    pub notes: String,
    /// `(path, field label)` to bump.
    pub fields: Vec<(String, String)>,
    pub changelog: bool,
    pub create_changelog: bool,
    pub use_unreleased: bool,
    pub message: String,
    pub date: String,
    /// Pick the default manifests and the changelog (script).
    pub auto: bool,
    /// Commit without asking (script, scratch repositories only).
    pub now: bool,
}

/// The release commit, ready to confirm.
#[derive(Clone, Debug)]
pub struct ReleasePlan {
    pub dir: PathBuf,
    pub version: String,
    pub tag: String,
    pub message: String,
    pub edits: Vec<hadion_core::devops::automation::release::FileEdit>,
}

#[derive(Clone, Debug)]
pub enum Act {
    // Notifications.
    SetEnabled(bool),
    SetPaused(bool),
    PollNow,
    /// Script: log a sample notification (only with `HADION_NOTIFY=log`).
    NotifyTest,
    /// Settings: send one test notification through the real delivery.
    SendTest,
    ShowList(bool),
    OpenEvent(String),
    ClearRecent,
    // Integrations.
    CheckSecrets,
    SaveSecret {
        name: &'static str,
        value: Secret,
    },
    RemoveSecret(&'static str),
    Lookup(Vec<(Tracker, String)>),
    RetryLookups,
    LinksTest(String),
    /// Script: set up Jira (`base`, project keys).
    SetJira {
        base: String,
        keys: Vec<String>,
    },
    /// Script: set up Linear (`workspace`, team keys).
    SetLinear {
        workspace: String,
        keys: Vec<String>,
    },
    /// Script: title lookups on or off (both trackers).
    SetLookup(bool),
    /// Script: store (or remove) fake credentials under `hadion-test-…`
    /// names (only with `HADION_KEYCHAIN_TEST` set).
    TestCredentials(bool),
    // Groups.
    GroupNew(String),
    GroupSelect(String),
    GroupRename {
        from: String,
        to: String,
    },
    GroupDelete(String),
    GroupDeleteConfirmed(String),
    GroupAdd {
        group: String,
        path: PathBuf,
    },
    GroupRemove {
        group: String,
        path: PathBuf,
    },
    GroupPickFolder(String),
    GroupOpenAll(String),
    GroupStatus,
    GroupBatch {
        group: String,
        batch: Batch,
    },
    GroupBatchConfirmed {
        group: String,
        batch: Batch,
        list: Vec<(String, PathBuf)>,
    },
    /// Script: run without the confirmation (scratch repositories only).
    GroupBatchNow {
        group: String,
        batch: Batch,
    },
    ClearBatch,
    // Release helpers.
    ReleaseOpen(bool),
    ReleaseDetect(PathBuf),
    ReleasePrepare(Box<PrepareReq>),
    ReleaseCommit(Box<ReleasePlan>),
    /// Script: prepare the release commit for `version` with the default
    /// choices; `now` commits without the confirmation.
    ReleaseAuto {
        version: String,
        now: bool,
    },
    SetReleaseTemplate(String),
    OpenPush,
    /// Script: the next automation page drawn scrolls to its end.
    ScrollEnd,
    /// Script: add a custom pattern (`name`, regex, URL template).
    AddCustomRule {
        name: String,
        pattern: String,
        url: String,
    },
}

/// The result of one poll.
pub struct PollDone {
    pub poll: hadion_core::devops::automation::notify::Poll,
    pub requests: usize,
    pub elapsed_ms: u64,
}

pub enum Msg {
    Polled(Box<PollDone>),
    Delivered(Result<String, String>),
    Secrets {
        jira: Result<bool, String>,
        linear: Result<bool, String>,
        curl: Option<PathBuf>,
    },
    SecretSaved {
        name: &'static str,
        stored: Result<bool, String>,
    },
    Looked(Vec<(Tracker, String, Result<IssueInfo, String>)>),
    Status(Vec<(PathBuf, Result<RepoStatus, String>)>),
    BatchDone {
        group: String,
        batch: Batch,
        results: Vec<RepoResult>,
        elapsed_ms: u64,
    },
    Detected {
        dir: PathBuf,
        result: Result<Discovered, String>,
    },
    Prepared {
        result: Result<ReleasePlan, String>,
        dir: PathBuf,
        now: bool,
    },
    Committed {
        dir: PathBuf,
        result: Result<String, String>,
    },
}

impl Msg {
    /// One line for the activity log (`Err` when the work failed).
    pub fn summary(&self) -> Result<String, String> {
        match self {
            Msg::Polled(d) => notifications::poll_summary(d),
            Msg::Delivered(r) => r.clone(),
            Msg::Secrets { jira, linear, .. } => {
                let s = |r: &Result<bool, String>| match r {
                    Ok(true) => "stored".to_owned(),
                    Ok(false) => "none".to_owned(),
                    Err(e) => e.clone(),
                };
                Ok(format!(
                    "Keychain: Jira token {}, Linear API key {}",
                    s(jira),
                    s(linear)
                ))
            }
            Msg::SecretSaved { name, stored } => match stored {
                Ok(true) => Ok(format!("Stored {name} in the keychain")),
                Ok(false) => Ok(format!("Removed {name} from the keychain")),
                Err(e) => Err(e.clone()),
            },
            Msg::Looked(v) => {
                let ok = v.iter().filter(|(_, _, r)| r.is_ok()).count();
                let lines: Vec<String> = v
                    .iter()
                    .map(|(_, k, r)| match r {
                        Ok(i) => format!("{k}: {} ({})", i.title, i.status),
                        Err(e) => format!("{k}: {e}"),
                    })
                    .collect();
                let text = format!("Looked up {ok} of {}\n{}", v.len(), lines.join("\n"));
                if ok == 0 && !v.is_empty() {
                    Err(text)
                } else {
                    Ok(text)
                }
            }
            Msg::Status(v) => {
                let bad = v.iter().filter(|(_, r)| r.is_err()).count();
                Ok(format!(
                    "Status of {} repositories{}",
                    v.len(),
                    if bad > 0 {
                        format!(", {bad} unreadable")
                    } else {
                        String::new()
                    }
                ))
            }
            Msg::BatchDone { batch, results, .. } => Ok(
                hadion_core::devops::automation::groups::summary(batch, results),
            ),
            Msg::Detected { result, .. } => match result {
                Ok((m, c)) => Ok(format!(
                    "{} version manifests{}",
                    m.len(),
                    if c.is_some() { ", CHANGELOG.md" } else { "" }
                )),
                Err(e) => Err(e.clone()),
            },
            Msg::Prepared { result, .. } => match result {
                Ok(p) => Ok(format!(
                    "Release commit for {}: {} files",
                    p.version,
                    p.edits.len()
                )),
                Err(e) => Err(e.clone()),
            },
            Msg::Committed { result, .. } => result.clone(),
        }
    }
}

/// Notification settings.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifyPrefs {
    /// Off by default: nothing polls until the user turns it on.
    pub enabled: bool,
    pub paused: bool,
    /// Minutes between checks (2, 5, 10 or 15).
    pub interval_min: u32,
    pub runs: RunWatch,
    pub reviews: bool,
    pub mentions: bool,
    pub approvals: bool,
    /// Only repositories open in Hadion.
    pub only_open: bool,
    pub quiet: bool,
    /// `22:00`.
    pub quiet_from: String,
    /// `07:00`.
    pub quiet_to: String,
}

impl Default for NotifyPrefs {
    fn default() -> Self {
        Self {
            enabled: false,
            paused: false,
            interval_min: 5,
            runs: RunWatch::Failures,
            reviews: true,
            mentions: true,
            approvals: true,
            only_open: true,
            quiet: false,
            quiet_from: "22:00".into(),
            quiet_to: "07:00".into(),
        }
    }
}

/// Release helper settings.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ReleasePrefs {
    /// Commit message template (`{version}`, `{tag}`).
    pub message: String,
}

impl Default for ReleasePrefs {
    fn default() -> Self {
        Self {
            message: "chore(release): v{version}".into(),
        }
    }
}

/// App-wide preferences (`Settings::ext`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub notify: NotifyPrefs,
    pub links: LinkPrefs,
    pub groups: Vec<Group>,
    pub release: ReleasePrefs,
}

pub fn apply(cx: &mut Ctx, st: &mut State, g: &mut GlobalCx, act: Act) {
    match act {
        Act::SetEnabled(_)
        | Act::SetPaused(_)
        | Act::PollNow
        | Act::NotifyTest
        | Act::SendTest
        | Act::ShowList(_)
        | Act::OpenEvent(_)
        | Act::ClearRecent => notifications::apply(cx, &mut st.notify, g, act),
        Act::CheckSecrets
        | Act::SaveSecret { .. }
        | Act::RemoveSecret(_)
        | Act::Lookup(_)
        | Act::RetryLookups
        | Act::LinksTest(_)
        | Act::SetJira { .. }
        | Act::SetLinear { .. }
        | Act::SetLookup(_)
        | Act::TestCredentials(_)
        | Act::AddCustomRule { .. } => integrations::apply(cx, &mut st.links, g, act),
        Act::ScrollEnd => shared::lock().scroll_end = true,
        Act::ReleaseOpen(_)
        | Act::ReleaseDetect(_)
        | Act::ReleasePrepare(_)
        | Act::ReleaseCommit(_)
        | Act::ReleaseAuto { .. }
        | Act::SetReleaseTemplate(_)
        | Act::OpenPush => release::apply(cx, g, act),
        _ => groups::apply(cx, g, act),
    }
}

pub fn on_msg(cx: &mut Ctx, st: &mut State, g: &mut GlobalCx, msg: Msg) {
    match msg {
        Msg::Polled(_) | Msg::Delivered(_) => notifications::on_msg(cx, &mut st.notify, g, msg),
        Msg::Secrets { .. } | Msg::SecretSaved { .. } | Msg::Looked(_) => {
            integrations::on_msg(cx, &mut st.links, g, msg)
        }
        Msg::Status(_) | Msg::BatchDone { .. } => groups::on_msg(cx, g, msg),
        Msg::Detected { .. } | Msg::Prepared { .. } | Msg::Committed { .. } => {
            release::on_msg(cx, g, msg)
        }
    }
}

/// Every frame; keep it cheap.
pub fn tick(cx: &mut Ctx, st: &mut State, g: &mut GlobalCx) {
    notifications::tick(cx, &mut st.notify, g);
}

/// `HADION_SCRIPT` commands this area understands: `links-jira BASE
/// [KEY,KEY]`, `links-linear WORKSPACE [KEY,KEY]`, `links-lookup on|off`,
/// `links-custom NAME|REGEX|URL`, `auto-scroll-end` (the next
/// automation page scrolls to its end), `links-test-token on|off` (fake credentials under `hadion-test-` names,
/// only with `HADION_KEYCHAIN_TEST=1`), `notify-on`,
/// `notify-off`, `notify-test` (logs a sample, only with
/// `HADION_NOTIFY=log`), `notify-poll`, `notify-list`, `groups`,
/// `group-new NAME`, `group-add NAME PATH`, `group-select NAME`,
/// `group-fetch NAME` / `group-pull NAME` / `group-branch NAME BRANCH [BASE]`
/// (stop at the confirmation), `group-run-now NAME fetch|pull|branch X`
/// (no confirmation; scratch repositories only), `links-test TEXT`,
/// `release-prep VERSION` (Ship → Releases → new release with that version,
/// the release section open), `release-review VERSION` (the confirmation with
/// the default choices), `release-commit-now VERSION` (no
/// confirmation; scratch repositories only).
pub fn script(cmd: &str, arg: &str) -> Option<Vec<Action>> {
    let arg = arg.trim();
    let groups_page = || {
        vec![
            Action::ShowPage(Page::Home),
            crate::devops::home::Act::SetTab(crate::devops::home::Tab::Groups).into(),
        ]
    };
    let two = || {
        let (a, b) = arg.split_once(' ')?;
        Some((a.trim().to_owned(), b.trim().to_owned()))
    };
    Some(match cmd {
        "notify-on" => vec![Act::SetEnabled(true).into()],
        "notify-off" => vec![Act::SetEnabled(false).into()],
        "notify-test" => vec![Act::NotifyTest.into()],
        "notify-poll" => vec![Act::PollNow.into()],
        "notify-list" => vec![Act::ShowList(true).into()],
        "groups" => groups_page(),
        "group-new" if !arg.is_empty() => {
            let mut v = groups_page();
            v.push(Act::GroupNew(arg.to_owned()).into());
            v
        }
        "group-select" if !arg.is_empty() => {
            let mut v = groups_page();
            v.push(Act::GroupSelect(arg.to_owned()).into());
            v
        }
        "group-add" => {
            let (group, path) = two()?;
            vec![
                Act::GroupAdd {
                    group,
                    path: PathBuf::from(path),
                }
                .into(),
            ]
        }
        "group-fetch" | "group-pull" if !arg.is_empty() => {
            let mut v = groups_page();
            v.push(
                Act::GroupBatch {
                    group: arg.to_owned(),
                    batch: if cmd == "group-fetch" {
                        Batch::Fetch
                    } else {
                        Batch::Pull
                    },
                }
                .into(),
            );
            v
        }
        "group-branch" => {
            let (group, rest) = two()?;
            let mut it = rest.split_whitespace();
            let name = it.next()?.to_owned();
            let base = it.next().map(str::to_owned);
            let mut v = groups_page();
            v.push(
                Act::GroupBatch {
                    group,
                    batch: Batch::Branch { name, base },
                }
                .into(),
            );
            v
        }
        "group-run-now" => {
            let (group, rest) = two()?;
            let mut it = rest.split_whitespace();
            let batch = match it.next()? {
                "fetch" => Batch::Fetch,
                "pull" => Batch::Pull,
                "branch" => Batch::Branch {
                    name: it.next()?.to_owned(),
                    base: it.next().map(str::to_owned),
                },
                _ => return None,
            };
            let mut v = groups_page();
            v.push(Act::GroupBatchNow { group, batch }.into());
            v
        }
        "links-jira" | "links-linear" if !arg.is_empty() => {
            let mut it = arg.split_whitespace();
            let first = it.next()?.to_owned();
            let keys: Vec<String> = it
                .next()
                .map(|k| {
                    k.split(',')
                        .map(|x| x.trim().to_owned())
                        .filter(|x| !x.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            vec![
                if cmd == "links-jira" {
                    Act::SetJira { base: first, keys }
                } else {
                    Act::SetLinear {
                        workspace: first,
                        keys,
                    }
                }
                .into(),
            ]
        }
        "links-lookup" => vec![Act::SetLookup(arg != "off").into()],
        "links-custom" => {
            let mut parts = arg.splitn(3, '|');
            let (name, pattern, url) = (parts.next()?, parts.next()?, parts.next()?);
            vec![
                Act::AddCustomRule {
                    name: name.trim().to_owned(),
                    pattern: pattern.trim().to_owned(),
                    url: url.trim().to_owned(),
                }
                .into(),
            ]
        }
        "auto-scroll-end" => vec![Act::ScrollEnd.into()],
        "links-test-token" => vec![Act::TestCredentials(arg != "off").into()],
        "links-test" => vec![
            crate::devops::DevOpsAction::SetSettingsSection(Section::Integrations).into(),
            Action::ShowPage(Page::Settings),
            Act::LinksTest(arg.to_owned()).into(),
        ],
        "release-prep" if !arg.is_empty() => {
            let tag =
                if arg.starts_with(['v', 'V']) || !arg.starts_with(|c: char| c.is_ascii_digit()) {
                    arg.to_owned()
                } else {
                    format!("v{arg}")
                };
            vec![
                Action::ShowPage(Page::Ship),
                crate::devops::DevOpsAction::SetShipTab(crate::devops::ShipTab::Releases).into(),
                crate::devops::ship::Act::NewRelease.into(),
                crate::devops::ship::Act::SetFormTag(tag).into(),
                Act::ReleaseOpen(true).into(),
            ]
        }
        "release-review" if !arg.is_empty() => vec![
            Act::ReleaseAuto {
                version: arg.to_owned(),
                now: false,
            }
            .into(),
        ],
        "release-commit-now" if !arg.is_empty() => {
            vec![
                Act::ReleaseAuto {
                    version: arg.to_owned(),
                    now: true,
                }
                .into(),
            ]
        }
        _ => return None,
    })
}

/// A Settings section became visible (check `section`; most are not yours).
pub fn on_section_shown(cx: &mut Ctx, st: &mut State, g: &mut GlobalCx, section: Section) {
    if section == Section::Integrations {
        integrations::apply(cx, &mut st.links, g, Act::CheckSecrets);
    }
}

/// Notifications and Integrations sections.
pub fn settings_section(
    ui: &mut egui::Ui,
    st: &mut State,
    settings: &mut Settings,
    repo: Option<&RepoState>,
    section: Section,
    actions: &mut Vec<Action>,
) {
    match section {
        Section::Notifications => notifications::settings(ui, &mut st.notify, settings, actions),
        _ => integrations::settings(ui, &mut st.links, settings, repo, actions),
    }
}

pub fn hook_commit_panel(ui: &mut egui::Ui, h: &HookCx, c: &Commit, actions: &mut Vec<Action>) {
    integrations::hook_commit_panel(ui, h, c, actions);
}

pub fn hook_wip_message_tools(ui: &mut egui::Ui, h: &HookCx, actions: &mut Vec<Action>) {
    integrations::hook_wip(ui, h, actions);
}

pub fn hook_pr_tools(ui: &mut egui::Ui, h: &HookCx, number: u64, actions: &mut Vec<Action>) {
    integrations::hook_pr(ui, h, number, actions);
}

/// Right side of the status bar (right-to-left).
pub fn hook_status_items(ui: &mut egui::Ui, st: &State, v: &GlobalView, actions: &mut Vec<Action>) {
    notifications::status_item(ui, &st.notify, v, actions);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefs_defaults_and_serde() {
        let p = Prefs::default();
        assert!(!p.notify.enabled, "notifications are opt-in");
        assert_eq!(p.notify.interval_min, 5);
        assert_eq!(p.notify.runs, RunWatch::Failures);
        assert!(p.notify.only_open);
        assert_eq!(p.release.message, "chore(release): v{version}");
        let json = serde_json::to_string(&p).unwrap();
        let back: Prefs = serde_json::from_str(&json).unwrap();
        assert_eq!(back.notify.quiet_from, "22:00");
        // Old settings files without the area's keys load with defaults.
        let empty: Prefs = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.notify.interval_min, 5);
        let partial: Prefs = serde_json::from_str(
            r#"{"groups":[{"name":"Work","repos":["C:/a"]}],"notify":{"enabled":true}}"#,
        )
        .unwrap();
        assert_eq!(partial.groups[0].name, "Work");
        assert!(partial.notify.enabled && partial.notify.reviews);
    }

    #[test]
    fn secrets_never_print() {
        let s = Secret::new("hunter2-token".into());
        assert_eq!(format!("{s:?}"), "Secret(•••)");
        let a = Act::SaveSecret {
            name: "jira-token",
            value: s,
        };
        assert!(!format!("{a:?}").contains("hunter2"));
    }

    #[test]
    fn script_commands() {
        assert_eq!(script("groups", "").unwrap().len(), 2);
        assert_eq!(script("group-new", "Work").unwrap().len(), 3);
        assert!(script("group-new", "").is_none());
        assert!(script("group-add", "Work").is_none());
        assert_eq!(script("group-add", "Work C:/src/a b").unwrap().len(), 1);
        match &script("group-branch", "Work release/1.2 origin/main").unwrap()[2] {
            Action::DevOps(crate::devops::DevOpsAction::Automation(Act::GroupBatch {
                group,
                batch: Batch::Branch { name, base },
            })) => {
                assert_eq!(group, "Work");
                assert_eq!(name, "release/1.2");
                assert_eq!(base.as_deref(), Some("origin/main"));
            }
            a => panic!("{a:?}"),
        }
        assert!(script("group-run-now", "Work push").is_none());
        assert_eq!(script("release-prep", "1.2.0").unwrap().len(), 5);
        match &script("release-prep", "1.2.0").unwrap()[3] {
            Action::DevOps(crate::devops::DevOpsAction::Ship(
                crate::devops::ship::Act::SetFormTag(t),
            )) => {
                assert_eq!(t, "v1.2.0")
            }
            a => panic!("{a:?}"),
        }
        assert_eq!(script("links-test", "ABC-1 fix").unwrap().len(), 3);
        assert!(script("notify-test", "").is_some());
        assert!(script("nope", "").is_none());
    }
}
