//! Git configuration and identities on the Settings page: git config
//! (system / global / repository) editor, identities per folder
//! (`includeIf`), SSH and GPG keys with commit signing setup, and the GitHub
//! CLI account (sign-in state, token scopes, fixes).
//!
//! One view file per section (`config_view`, `identities_view`,
//! `keys_view`, `account_view`); this file holds the contract the
//! dispatcher calls: state, actions, replies, loads and changes. Every
//! change is a list of core [`Step`]s shown in a confirmation and run by
//! `identity::run_steps` in the background.

mod account_view;
mod config_view;
mod identities_view;
mod keys_view;
mod widgets;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gitvis_core::devops::identity::account::{self, Report as AccountReport};
use gitvis_core::devops::identity::config::{self, Snapshot};
use gitvis_core::devops::identity::gpg;
use gitvis_core::devops::identity::includes::{self, Applied, NewIdentity, Rule};
use gitvis_core::devops::identity::signing::{self, Format};
use gitvis_core::devops::identity::ssh::{self, Agent, KeyUse, PubKey, TestResult};
use gitvis_core::devops::identity::{
    self as core, Scope, Step, display_steps, git_home, global_config_file, slash, ssh_dir, tilde,
};
use gitvis_core::devops::proc;
use serde::{Deserialize, Serialize};

use crate::devops::DevOpsAction;
use crate::devops::ctx::{Ctx, TaskLog};
use crate::devops::global::{GlobalCx, GlobalView};
use crate::devops::settings_page::Section;
use crate::state::{Action, Page, RepoState, Settings, ToastKind};

/// Something loaded in the background: the last result, whether a load is
/// running, and when the result came.
pub struct Loaded<T> {
    pub data: Option<Result<T, String>>,
    pub loading: bool,
    pub at: Option<Instant>,
}

impl<T> Default for Loaded<T> {
    fn default() -> Self {
        Self {
            data: None,
            loading: false,
            at: None,
        }
    }
}

impl<T> Loaded<T> {
    /// Mark a load as started; false when one is running already.
    fn start(&mut self) -> bool {
        if self.loading {
            return false;
        }
        self.loading = true;
        true
    }

    fn done(&mut self, r: Result<T, String>) {
        self.loading = false;
        self.at = Some(Instant::now());
        self.data = Some(r);
    }

    pub fn ok(&self) -> Option<&T> {
        match &self.data {
            Some(Ok(t)) => Some(t),
            _ => None,
        }
    }
}

/// Identity rules and what applies in every open repository.
#[derive(Clone, Debug, Default)]
pub struct IdentityReport {
    pub rules: Vec<Rule>,
    /// `(repository name, what applies there)`.
    pub repos: Vec<(String, Applied)>,
    pub global_file: Option<PathBuf>,
    pub home: Option<PathBuf>,
}

/// SSH keys, tools and the agent.
#[derive(Clone, Debug, Default)]
pub struct KeysReport {
    pub ssh_dir: Option<PathBuf>,
    pub dir_exists: bool,
    pub keys: Vec<PubKey>,
    pub keygen: Option<PathBuf>,
    pub ssh: Option<PathBuf>,
    pub ssh_add: Option<PathBuf>,
    /// `None` without ssh-add.
    pub agent: Option<Agent>,
    /// github.com in known_hosts (`None`: hashed entries, cannot tell).
    pub github_known: Option<bool>,
}

/// What `ssh -T git@github.com` said.
#[derive(Clone, Debug)]
pub struct SshTest {
    pub result: TestResult,
    pub command: String,
    pub ms: u64,
}

/// The GitHub account section's data.
#[derive(Clone, Debug, Default)]
pub struct AccountData {
    /// Where gh is (`None`: not installed).
    pub gh: Option<PathBuf>,
    pub report: AccountReport,
}

/// The Git config section's tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ConfigTab {
    #[default]
    Common,
    All,
    Files,
}

impl ConfigTab {
    pub fn name(self) -> &'static str {
        match self {
            ConfigTab::Common => "common",
            ConfigTab::All => "all",
            ConfigTab::Files => "files",
        }
    }

    pub fn from_name(s: &str) -> Option<ConfigTab> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "common" | "settings" => ConfigTab::Common,
            "all" | "entries" => ConfigTab::All,
            "files" => ConfigTab::Files,
            _ => return None,
        })
    }
}

/// The inline config editor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditForm {
    pub key: String,
    pub scope: Scope,
    pub value: String,
}

/// "Add a key" on the All entries tab.
#[derive(Clone, Debug, Default)]
pub struct RawForm {
    pub key: String,
    pub value: String,
    pub error: String,
}

/// The add / edit identity form.
#[derive(Clone, Debug, Default)]
pub struct IdentityForm {
    /// The rule being edited (index into the report's rules).
    pub editing: Option<usize>,
    pub folder: String,
    pub icase: bool,
    pub name: String,
    pub email: String,
    pub signing_key: String,
    pub slug: String,
    /// The slug follows the folder until it is typed over.
    pub slug_auto: bool,
    pub error: String,
}

/// The new-key form.
#[derive(Clone, Debug, Default)]
pub struct GenerateForm {
    pub email: String,
    pub path: String,
    pub passphrase: bool,
    pub error: String,
}

/// The signing setup form.
#[derive(Clone, Debug)]
pub struct SigningForm {
    pub format: Format,
    /// The selected SSH key (its `.pub` file).
    pub ssh_key: Option<PathBuf>,
    /// The selected GPG key id.
    pub gpg_key: Option<String>,
    /// Point `user.signingkey` at the private key file.
    pub use_private: bool,
    pub scope: Scope,
    pub tags: bool,
    pub email: String,
    pub error: String,
}

impl Default for SigningForm {
    fn default() -> Self {
        Self {
            format: Format::Ssh,
            ssh_key: None,
            gpg_key: None,
            use_private: true,
            scope: Scope::Global,
            tags: true,
            email: String::new(),
            error: String::new(),
        }
    }
}

/// What a key upload is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadTarget {
    Ssh(PathBuf),
    Gpg(String),
}

/// The upload form, shown under the key.
#[derive(Clone, Debug)]
pub struct UploadForm {
    pub target: UploadTarget,
    pub title: String,
    pub key_use: KeyUse,
}

#[derive(Default)]
pub struct State {
    pub config: Loaded<Snapshot>,
    pub identities: Loaded<IdentityReport>,
    pub keys: Loaded<KeysReport>,
    pub gpg: Loaded<gpg::Report>,
    pub account: Loaded<AccountData>,
    pub ssh_test: Loaded<SshTest>,
    pub check: Loaded<Applied>,
    pub filter: String,
    pub edit: Option<EditForm>,
    pub raw: RawForm,
    pub id_form: Option<IdentityForm>,
    pub check_path: String,
    pub gen_form: GenerateForm,
    pub sign: SigningForm,
    pub upload: Option<UploadForm>,
    /// A change is running.
    pub busy: bool,
    /// Scroll the shown section to this offset once (scripts).
    pub scroll: Option<f32>,
    /// Open repositories when the section was shown: `(name, folder)`.
    pub repos: Vec<(String, PathBuf)>,
}

/// Which sections to load again after a change (the config always is).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reload {
    pub identities: bool,
    pub keys: bool,
    pub account: bool,
}

impl Reload {
    const CONFIG: Reload = Reload {
        identities: true,
        keys: true,
        account: false,
    };
}

#[derive(Clone, Debug)]
pub enum Act {
    /// Load a section again.
    Refresh(Section),
    SetConfigTab(ConfigTab),
    SetFilter(String),
    /// Open the inline editor for a key.
    EditKey(EditForm),
    CloseEditor,
    /// Ask to set a key (shows the confirmation).
    SetConfig {
        scope: Scope,
        key: String,
        value: String,
    },
    UnsetConfig {
        scope: Scope,
        key: String,
        value: Option<String>,
    },
    /// Set the raw key typed in "Add a key".
    AddRawKey(Scope),
    /// Run confirmed steps.
    RunPlan {
        label: String,
        steps: Vec<Step>,
        reload: Reload,
    },
    /// Open the identity form, for a new identity or to edit a rule.
    OpenIdentityForm(Option<usize>),
    CloseIdentityForm,
    /// Fill the form (scripts): folder, name, e-mail.
    FillIdentity {
        folder: String,
        name: String,
        email: String,
    },
    PickIdentityFolder,
    SubmitIdentity,
    RemoveIdentity {
        rule: usize,
        delete_file: bool,
    },
    /// Put a folder in the check box (scripts).
    SetCheckPath(String),
    /// Check which identity applies at the typed folder.
    CheckPath,
    TestSsh,
    GenerateKey,
    /// Pick the signing format and its first key.
    SigningKind(Format),
    /// Pick an SSH key (index) for signing.
    SigningSsh(usize),
    /// Pick a GPG key (index) for signing.
    SigningGpg(usize),
    SubmitSigning,
    OpenUpload(UploadTarget),
    /// Open the upload form for the N-th SSH key (scripts).
    OpenUploadSsh(usize),
    CloseUpload,
    SubmitUpload,
    SwitchAccount {
        host: String,
        login: String,
    },
    SetupGit,
    /// Scroll the shown section (scripts).
    Scroll(f32),
}

pub enum Msg {
    Config(Result<Snapshot, String>),
    Identities(Box<Result<(Snapshot, IdentityReport), String>>),
    Keys(Box<KeysReport>),
    Gpg(Box<gpg::Report>),
    Account(Box<AccountData>),
    SshTest(Result<SshTest, String>),
    Check(Box<Applied>),
    Changed(Reload),
}

impl Msg {
    /// One line for the activity log (`Err` when the work failed).
    pub fn summary(&self) -> Result<String, String> {
        match self {
            Msg::Config(Ok(s)) => Ok(format!(
                "{} config entries from {} files · {} ms",
                s.entries.len(),
                s.files.len(),
                s.elapsed_ms
            )),
            Msg::Config(Err(e)) => Err(e.clone()),
            Msg::Identities(r) => match r.as_ref() {
                Ok((_, rep)) => Ok(format!(
                    "{} identity rules · {} repositories checked",
                    rep.rules.len(),
                    rep.repos.len()
                )),
                Err(e) => Err(e.clone()),
            },
            Msg::Keys(k) => Ok(format!(
                "{} SSH keys in {} · agent: {}",
                k.keys.len(),
                k.ssh_dir.as_deref().map(slash).unwrap_or_default(),
                match &k.agent {
                    Some(Agent::Keys(v)) => format!("{} keys", v.len()),
                    Some(Agent::Empty) => "no keys".to_owned(),
                    Some(Agent::NotRunning(_)) => "not running".to_owned(),
                    Some(Agent::Error(e)) => e.clone(),
                    None => "ssh-add not found".to_owned(),
                }
            )),
            Msg::Gpg(g) => match (&g.found, &g.error) {
                (None, _) => Ok("gpg is not installed".to_owned()),
                (Some(_), Some(e)) => Err(e.clone()),
                (Some(f), None) => Ok(format!(
                    "{} secret keys · {}",
                    g.keys.len(),
                    slash(&f.program)
                )),
            },
            Msg::Account(a) => {
                let r = &a.report;
                if a.gh.is_none() {
                    Ok("gh is not installed".to_owned())
                } else if let Some(e) = &r.status_error {
                    Err(e.clone())
                } else if r.signed_out {
                    Ok("Not signed in to gh".to_owned())
                } else {
                    Ok(format!(
                        "{} accounts · active: {}",
                        r.accounts.len(),
                        r.active().map(|a| a.login.as_str()).unwrap_or("none")
                    ))
                }
            }
            Msg::SshTest(Ok(t)) => match &t.result {
                TestResult::Authenticated(u) => Ok(format!("GitHub knows this key: {u}")),
                other => Err(test_text(other).to_owned()),
            },
            Msg::SshTest(Err(e)) => Err(e.clone()),
            Msg::Check(a) => match &a.error {
                Some(e) => Err(e.clone()),
                None => Ok(format!(
                    "{}: {}",
                    slash(&a.path),
                    a.email
                        .as_ref()
                        .map(|e| e.value_text().to_owned())
                        .unwrap_or_else(|| "no e-mail".to_owned())
                )),
            },
            Msg::Changed(_) => Ok(String::new()),
        }
    }
}

fn test_text(r: &TestResult) -> &str {
    match r {
        TestResult::Authenticated(u) => u,
        TestResult::Denied(t)
        | TestResult::HostKey(t)
        | TestResult::Network(t)
        | TestResult::Failed(t) => t,
    }
}

/// App-wide preferences (`Settings::ext`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// The scope last used for an edit (global or local).
    pub scope: Scope,
    /// Collapsed groups on the All entries tab (`remote.origin`).
    pub collapsed: Vec<String>,
    /// `common`, `all` or `files`.
    pub config_tab: String,
    /// New identities use `gitdir/i:` (case-insensitive).
    pub icase: bool,
    /// The folder last checked under "Which identity applies".
    pub check_path: String,
    /// Sign tags too in the signing form.
    pub sign_tags: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            scope: Scope::Global,
            collapsed: Vec::new(),
            config_tab: ConfigTab::Common.name().to_owned(),
            icase: cfg!(windows),
            check_path: String::new(),
            sign_tags: true,
        }
    }
}

impl Prefs {
    pub fn tab(&self) -> ConfigTab {
        ConfigTab::from_name(&self.config_tab).unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Loads
// ---------------------------------------------------------------------------

fn load_config(cx: &mut Ctx, st: &mut State) {
    if !st.config.start() {
        return;
    }
    if !cx.read("Read git config", |dir, log| {
        Msg::Config(config::load(dir, &mut log.commands))
    }) {
        st.config.done(Err("Open a repository first".to_owned()));
    }
}

fn load_identities(cx: &mut Ctx, st: &mut State) {
    if !st.identities.start() {
        return;
    }
    let repos = st.repos.clone();
    if !cx.read("Read identities", move |dir, log| {
        let home = git_home();
        let r = config::load(dir, &mut log.commands).map(|snap| {
            let mut rules = includes::rules(&snap.entries, home.as_deref());
            includes::read_targets(&mut rules, dir, &mut log.commands);
            let repos = repos
                .iter()
                .map(|(name, path)| {
                    (
                        name.clone(),
                        includes::applied(
                            path,
                            &rules,
                            &snap.entries,
                            home.as_deref(),
                            &mut log.commands,
                        ),
                    )
                })
                .collect();
            let report = IdentityReport {
                rules,
                repos,
                global_file: global_config_file(),
                home,
            };
            (snap, report)
        });
        Msg::Identities(Box::new(r))
    }) {
        st.identities
            .done(Err("Open a repository first".to_owned()));
    }
}

fn load_keys(cx: &mut Ctx, st: &mut State) {
    if !st.keys.start() {
        return;
    }
    cx.read("Read SSH keys", |dir, log| {
        let ssh_dir = ssh_dir();
        let keygen = proc::resolve("ssh-keygen");
        let kg = keygen.as_ref().map(|p| p.to_string_lossy().into_owned());
        let keys = ssh_dir
            .as_deref()
            .map(|d| ssh::list(d, kg.as_deref(), &mut log.commands))
            .unwrap_or_default();
        let ssh_add = proc::resolve("ssh-add");
        let agent = ssh_add.as_ref().map(|p| {
            match core::tool(
                &p.to_string_lossy(),
                &["-l".to_owned()],
                dir,
                Duration::from_secs(10),
                &mut log.commands,
            ) {
                Ok(c) => ssh::parse_agent(c.code, &c.stdout_text(), &c.stderr),
                Err(e) => Agent::Error(e),
            }
        });
        Msg::Keys(Box::new(KeysReport {
            dir_exists: ssh_dir.as_deref().is_some_and(|d| d.is_dir()),
            github_known: ssh_dir
                .as_deref()
                .and_then(|d| ssh::known_host(&d.join("known_hosts"), "github.com")),
            ssh_dir,
            keys,
            keygen,
            ssh: proc::resolve("ssh"),
            ssh_add,
            agent,
        }))
    });
}

fn load_gpg(cx: &mut Ctx, st: &mut State) {
    if !st.gpg.start() {
        return;
    }
    cx.read("Read GPG keys", |dir, log| {
        let program = core::git_raw(dir, &["config", "--get", "gpg.program"], &mut log.commands)
            .ok()
            .filter(|o| o.success)
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
        Msg::Gpg(Box::new(gpg::load(
            dir,
            program.as_deref(),
            &mut log.commands,
        )))
    });
}

fn load_account(cx: &mut Ctx, st: &mut State) {
    if !st.account.start() {
        return;
    }
    cx.user_read("Read the GitHub CLI account", |gh, log| {
        let path = proc::resolve(&gitvis_core::github::gh_program());
        let report = if path.is_some() {
            account::load(gh, &mut log.calls)
        } else {
            AccountReport::default()
        };
        Msg::Account(Box::new(AccountData { gh: path, report }))
    });
}

fn remember_repos(st: &mut State, g: &GlobalCx) {
    st.repos = g
        .repos
        .iter()
        .map(|r| (r.name.clone(), r.workdir.clone()))
        .collect();
}

fn reload_section(cx: &mut Ctx, st: &mut State, section: Section) {
    match section {
        Section::GitConfig => load_config(cx, st),
        Section::Identities => load_identities(cx, st),
        Section::Keys => {
            load_config(cx, st);
            load_keys(cx, st);
            load_gpg(cx, st);
        }
        Section::GitHubAccount => {
            load_config(cx, st);
            load_account(cx, st);
        }
        _ => {}
    }
}

/// The effective value of a key from the last listing.
fn effective(st: &State, key: &str) -> Option<String> {
    st.config
        .ok()
        .and_then(|s| s.effective(key))
        .map(|e| e.value_text().to_owned())
}

fn default_email(st: &State) -> String {
    effective(st, "user.email").unwrap_or_default()
}

/// "the global config (~/.gitconfig)" or "gitvis's own config".
fn place(scope: Scope, g: &GlobalCx) -> String {
    match scope {
        Scope::Global => format!(
            "the global config ({})",
            global_config_file()
                .map(|f| tilde(&f, git_home().as_deref()))
                .unwrap_or_else(|| "~/.gitconfig".to_owned())
        ),
        Scope::Local => format!(
            "{}'s own config (.git/config)",
            g.active_repo()
                .map(|r| r.name.as_str())
                .unwrap_or("this repository")
        ),
        s => format!("the {} config", s.label()),
    }
}

fn run_plan(cx: &mut Ctx, st: &mut State, label: String, steps: Vec<Step>, reload: Reload) {
    if st.busy || steps.is_empty() {
        return;
    }
    st.busy = true;
    let remote = steps.iter().any(Step::remote);
    let job = move |dir: &std::path::Path, log: &mut TaskLog| {
        let mut calls = Vec::new();
        let r = core::run_steps(&steps, dir, &mut log.commands, &mut calls);
        log.calls.extend(calls);
        (r, Some(Msg::Changed(reload)))
    };
    if remote {
        cx.remote_change(label, job);
    } else if !cx.local_change(label, job) {
        st.busy = false;
        cx.toast(ToastKind::Error, "Open a repository first.");
    }
}

/// What a confirmation shows and runs.
struct Confirm<'a> {
    title: &'a str,
    body: String,
    button: &'a str,
    danger: bool,
    label: &'a str,
    steps: Vec<Step>,
    /// More monospace blocks after the commands (file contents).
    extra: Vec<String>,
    reload: Reload,
}

fn confirm_plan(cx: &mut Ctx, c: Confirm) {
    let mut details = vec![display_steps(&c.steps).join("\n")];
    details.extend(c.extra);
    cx.confirm(
        c.title,
        c.body,
        c.button,
        c.danger,
        details,
        Act::RunPlan {
            label: c.label.to_owned(),
            steps: c.steps,
            reload: c.reload,
        }
        .into(),
    );
}

// ---------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------

pub fn apply(cx: &mut Ctx, st: &mut State, g: &mut GlobalCx, act: Act) {
    match act {
        Act::Scroll(y) => st.scroll = Some(y),
        Act::Refresh(section) => {
            remember_repos(st, g);
            reload_section(cx, st, section);
        }
        Act::SetConfigTab(t) => g.settings.ext.identity.config_tab = t.name().to_owned(),
        Act::SetFilter(f) => st.filter = f,
        Act::EditKey(mut f) => {
            // Start from the value in that scope, else the one git uses.
            if f.value.is_empty()
                && let Some(s) = st.config.ok()
            {
                let k = config::norm_key(&f.key);
                f.value = s
                    .entries
                    .iter()
                    .rev()
                    .find(|e| e.key == k && e.scope == f.scope)
                    .or_else(|| s.effective(&k))
                    .map(|e| e.value_text().to_owned())
                    .unwrap_or_default();
            }
            st.edit = Some(f);
        }
        Act::CloseEditor => st.edit = None,
        Act::SetConfig { scope, key, value } => set_config(cx, st, g, scope, key, value),
        Act::UnsetConfig { scope, key, value } => unset_config(cx, st, g, scope, key, value),
        Act::AddRawKey(scope) => {
            let key = st.raw.key.trim().to_owned();
            let value = st.raw.value.clone();
            match config::set_args(scope, &key, &value) {
                Ok(_) => {
                    st.raw.error.clear();
                    set_config(cx, st, g, scope, key, value);
                }
                Err(e) => st.raw.error = e,
            }
        }
        Act::RunPlan {
            label,
            steps,
            reload,
        } => run_plan(cx, st, label, steps, reload),
        Act::OpenIdentityForm(rule) => open_identity_form(st, g, rule),
        Act::CloseIdentityForm => st.id_form = None,
        Act::FillIdentity {
            folder,
            name,
            email,
        } => {
            if st.id_form.is_none() {
                open_identity_form(st, g, None);
            }
            if let Some(f) = st.id_form.as_mut() {
                if !folder.is_empty() {
                    f.slug = includes::slug_for(&folder);
                    f.folder = folder;
                }
                if !name.is_empty() {
                    f.name = name;
                }
                if !email.is_empty() {
                    f.email = email;
                }
            }
        }
        Act::PickIdentityFolder => pick_folder(st),
        Act::SubmitIdentity => submit_identity(cx, st),
        Act::RemoveIdentity { rule, delete_file } => remove_identity(cx, st, rule, delete_file),
        Act::SetCheckPath(p) => st.check_path = p,
        Act::CheckPath => check_path(cx, st, g),
        Act::TestSsh => test_ssh(cx, st),
        Act::GenerateKey => generate_key(cx, st),
        Act::SigningKind(f) => {
            st.sign.format = f;
            st.sign.error.clear();
            match f {
                Format::Ssh if st.sign.ssh_key.is_none() => {
                    st.sign.ssh_key = st
                        .keys
                        .ok()
                        .and_then(|k| k.keys.iter().find(|k| k.has_private))
                        .map(|k| k.file.clone());
                }
                Format::Gpg if st.sign.gpg_key.is_none() => {
                    st.sign.gpg_key = st
                        .gpg
                        .ok()
                        .and_then(|r| r.keys.iter().find(|k| k.can_sign()))
                        .map(|k| k.keyid.clone());
                }
                _ => {}
            }
            if st.sign.email.is_empty() {
                st.sign.email = default_email(st);
            }
        }
        Act::SigningSsh(i) => {
            if let Some(k) = st.keys.ok().and_then(|r| r.keys.get(i)) {
                st.sign.format = Format::Ssh;
                st.sign.ssh_key = Some(k.file.clone());
                st.sign.error.clear();
                if st.sign.email.is_empty() {
                    st.sign.email = default_email(st);
                }
            }
        }
        Act::SigningGpg(i) => {
            if let Some(k) = st.gpg.ok().and_then(|r| r.keys.get(i)) {
                st.sign.format = Format::Gpg;
                st.sign.gpg_key = Some(k.keyid.clone());
                st.sign.error.clear();
            }
        }
        Act::SubmitSigning => submit_signing(cx, st, g),
        Act::OpenUpload(target) => open_upload(st, target),
        Act::OpenUploadSsh(i) => {
            if let Some(k) = st.keys.ok().and_then(|r| r.keys.get(i)) {
                let t = UploadTarget::Ssh(k.file.clone());
                open_upload(st, t);
            }
        }
        Act::CloseUpload => st.upload = None,
        Act::SubmitUpload => submit_upload(cx, st, g),
        Act::SwitchAccount { host, login } => confirm_plan(
            cx,
            Confirm {
                title: &format!("Switch gh to {login}?"),
                body: format!(
                    "gh uses {login} for {host} from now on: in gitvis, in your terminals, and for git over HTTPS when gh is the credential helper. Switch back the same way."
                ),
                button: "Switch account",
                danger: false,
                label: &format!("Switch gh to {login}"),
                steps: vec![Step::Gh(account::switch_args(&host, &login))],
                extra: Vec::new(),
                reload: Reload {
                    account: true,
                    ..Default::default()
                },
            },
        ),
        Act::SetupGit => confirm_plan(
            cx,
            Confirm {
                title: "Make gh git's credential helper for GitHub?",
                body: "gh adds credential.https://github.com.helper (and the one for gist.github.com) to your global config, so git over HTTPS signs in with gh's token instead of asking. Other hosts keep their helpers.".to_owned(),
                button: "Set up",
                danger: false,
                label: "gh auth setup-git",
                steps: vec![Step::Gh(account::setup_git_args())],
                extra: Vec::new(),
                reload: Reload {
                    account: true,
                    ..Default::default()
                },
            },
        ),
    }
}

fn set_config(
    cx: &mut Ctx,
    st: &mut State,
    g: &mut GlobalCx,
    scope: Scope,
    key: String,
    value: String,
) {
    let args = match config::set_args(scope, &key, &value) {
        Ok(a) => a,
        Err(e) => {
            cx.toast(ToastKind::Error, e);
            return;
        }
    };
    g.settings.ext.identity.scope = scope;
    let mut body = if config::is_multi(&key) {
        format!("Adds {value} to the list {key} in {}.", place(scope, g))
    } else {
        format!("Sets {key} to {value} in {}.", place(scope, g))
    };
    if let Some(snap) = st.config.ok()
        && !config::is_multi(&key)
    {
        let later: Vec<String> = snap
            .values(&key)
            .iter()
            .filter(|e| e.scope > scope && e.scope != Scope::Unknown)
            .map(|e| format!("{} ({})", e.scope.label(), e.value_text()))
            .collect();
        if !later.is_empty() {
            body.push_str(&format!(
                " In this repository it stays overridden by the {} value: git reads that scope later.",
                later.join(", ")
            ));
        }
    }
    confirm_plan(
        cx,
        Confirm {
            title: "Change git config?",
            body,
            button: "Set",
            danger: false,
            label: &format!("Set {key}"),
            steps: vec![Step::Git(args)],
            extra: Vec::new(),
            reload: Reload::CONFIG,
        },
    );
}

fn unset_config(
    cx: &mut Ctx,
    st: &mut State,
    g: &mut GlobalCx,
    scope: Scope,
    key: String,
    value: Option<String>,
) {
    let args = match config::unset_args(scope, &key, value.as_deref()) {
        Ok(a) => a,
        Err(e) => {
            cx.toast(ToastKind::Error, e);
            return;
        }
    };
    let fallback = st.config.ok().and_then(|s| {
        s.values(&key)
            .into_iter()
            .rev()
            .find(|e| e.scope != scope)
            .map(|e| format!("{} from the {} config", e.value_text(), e.scope.label()))
    });
    let body = format!(
        "Removes {key} from {}. {}",
        place(scope, g),
        match fallback {
            Some(f) => format!("git then uses {f}."),
            None => "git then uses its default.".to_owned(),
        }
    );
    confirm_plan(
        cx,
        Confirm {
            title: "Remove the setting?",
            body,
            button: "Unset",
            danger: true,
            label: &format!("Unset {key}"),
            steps: vec![Step::Git(args)],
            extra: Vec::new(),
            reload: Reload::CONFIG,
        },
    );
}

fn open_identity_form(st: &mut State, g: &GlobalCx, rule: Option<usize>) {
    let icase = g.settings.ext.identity.icase;
    let existing = rule.and_then(|i| {
        st.identities
            .ok()
            .and_then(|r| r.rules.get(i))
            .map(|r| (i, r.clone()))
    });
    let form = match existing {
        Some((i, r)) => IdentityForm {
            editing: Some(i),
            folder: r.condition.pattern().to_owned(),
            icase: matches!(r.condition, includes::Condition::GitDir { icase: true, .. }),
            name: r.identity.name.clone().unwrap_or_default(),
            email: r.identity.email.clone().unwrap_or_default(),
            signing_key: r.identity.signing_key.clone().unwrap_or_default(),
            slug: String::new(),
            slug_auto: false,
            error: String::new(),
        },
        None => IdentityForm {
            editing: None,
            icase,
            name: effective(st, "user.name").unwrap_or_default(),
            slug_auto: true,
            slug: "work".to_owned(),
            ..Default::default()
        },
    };
    st.id_form = Some(form);
}

fn pick_folder(st: &mut State) {
    let start = st
        .id_form
        .as_ref()
        .map(|f| f.folder.clone())
        .filter(|f| !f.trim().is_empty())
        .map(|f| PathBuf::from(core::from_msys(&f)))
        .or_else(git_home);
    let mut d = rfd::FileDialog::new().set_title("Folder whose repositories use this identity");
    if let Some(s) = start.filter(|s| s.is_dir()) {
        d = d.set_directory(s);
    }
    if let Some(p) = d.pick_folder()
        && let Some(f) = st.id_form.as_mut()
    {
        f.folder = slash(&p);
        if f.slug_auto {
            f.slug = includes::slug_for(&f.folder);
        }
    }
}

/// Where identity files go: next to the global config.
fn identity_dir() -> PathBuf {
    global_config_file()
        .and_then(|f| f.parent().map(PathBuf::from))
        .or_else(git_home)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The identity file the form would write.
pub fn form_file(form: &IdentityForm) -> PathBuf {
    let slug = gitvis_core::devops::slug(&form.slug, 40);
    includes::identity_file(
        &identity_dir(),
        if slug.is_empty() { "identity" } else { &slug },
    )
}

fn submit_identity(cx: &mut Ctx, st: &mut State) {
    let Some(form) = st.id_form.as_mut() else {
        return;
    };
    let Some(rep) = st.identities.ok() else {
        form.error = "The identities are still loading.".to_owned();
        return;
    };
    let home = rep.home.clone();
    let global = global_config_file().unwrap_or_else(|| identity_dir().join(".gitconfig"));
    let new = NewIdentity {
        folder: form.folder.clone(),
        icase: form.icase,
        name: form.name.clone(),
        email: form.email.clone(),
        signing_key: form.signing_key.clone(),
        file: form_file(form),
    };
    let plan = match form.editing.and_then(|i| rep.rules.get(i)) {
        Some(r) => includes::plan_edit(r, &new, &global),
        None => includes::plan_add(&new, &rep.rules, &global, home.as_deref()),
    };
    let plan = match plan {
        Ok(p) => p,
        Err(e) => {
            form.error = e;
            return;
        }
    };
    form.error.clear();
    let editing = form.editing.is_some();
    let h = home.as_deref();
    let file = tilde(&plan.file, h);
    let who = format!("{} <{}>", new.name.trim(), new.email.trim());
    let mut body = if editing {
        format!(
            "Updates {file}. Repositories under {} commit as {who}.",
            plan.condition.pattern()
        )
    } else {
        format!(
            "Writes {file} and adds a rule to {}, so every repository under {} commits as {who}. Repositories elsewhere keep your global identity.",
            tilde(&plan.global_file, h),
            plan.condition.pattern()
        )
    };
    if plan.file_exists && !editing {
        body.push_str(
            " The file exists already: its name and e-mail are replaced, other settings in it stay.",
        );
    }
    let mut extra = vec![format!(
        "{file} ({}):\n{}",
        if plan.file_exists {
            "its user section after the change"
        } else {
            "new file"
        },
        plan.file_text
    )];
    if !plan.global_text.is_empty() {
        extra.push(format!(
            "{} gets:\n{}",
            tilde(&plan.global_file, h),
            plan.global_text
        ));
    }
    confirm_plan(
        cx,
        Confirm {
            title: if editing {
                "Change the identity?"
            } else {
                "Add the identity?"
            },
            body,
            button: if editing { "Save" } else { "Add identity" },
            danger: false,
            label: if editing {
                "Edit identity"
            } else {
                "Add identity"
            },
            steps: plan.steps,
            extra,
            reload: Reload::CONFIG,
        },
    );
}

fn remove_identity(cx: &mut Ctx, st: &mut State, rule: usize, delete_file: bool) {
    let Some(rep) = st.identities.ok() else {
        return;
    };
    let Some(r) = rep.rules.get(rule) else {
        return;
    };
    let global = rep
        .global_file
        .clone()
        .unwrap_or_else(|| identity_dir().join(".gitconfig"));
    let steps = includes::plan_remove(r, delete_file, &global);
    let body = format!(
        "Repositories under {} use your global name and e-mail again{}.",
        r.condition.pattern(),
        if delete_file && r.exists {
            format!(", and {} is deleted", tilde(&r.target, rep.home.as_deref()))
        } else {
            String::new()
        }
    );
    confirm_plan(
        cx,
        Confirm {
            title: "Remove the identity?",
            body,
            button: "Remove",
            danger: true,
            label: "Remove identity",
            steps,
            extra: Vec::new(),
            reload: Reload::CONFIG,
        },
    );
}

fn check_path(cx: &mut Ctx, st: &mut State, g: &mut GlobalCx) {
    let path = st.check_path.trim().trim_matches('"').to_owned();
    if path.is_empty() {
        return;
    }
    g.settings.ext.identity.check_path = path.clone();
    let Some(rep) = st.identities.ok() else {
        cx.toast(ToastKind::Error, "The identities are still loading.");
        return;
    };
    let rules = rep.rules.clone();
    let home = rep.home.clone();
    let base: Vec<config::Entry> = st
        .config
        .ok()
        .map(|s| s.entries.clone())
        .unwrap_or_default();
    if !st.check.start() {
        return;
    }
    cx.read("Check which identity applies", move |_, log| {
        let p = PathBuf::from(core::from_msys(&path));
        Msg::Check(Box::new(includes::applied(
            &p,
            &rules,
            &base,
            home.as_deref(),
            &mut log.commands,
        )))
    });
}

fn test_ssh(cx: &mut Ctx, st: &mut State) {
    if !st.ssh_test.start() {
        return;
    }
    cx.read("Test SSH access to GitHub", |dir, log| {
        let Some(ssh) = proc::resolve("ssh") else {
            return Msg::SshTest(Err("ssh is not installed (not found on PATH)".to_owned()));
        };
        // Tests point known_hosts at a scratch file.
        let kh = std::env::var("GITVIS_SSH_KNOWN_HOSTS")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let args = ssh::test_args(kh.as_deref());
        let r = core::tool(
            &ssh.to_string_lossy(),
            &args,
            dir,
            Duration::from_secs(25),
            &mut log.commands,
        )
        .map(|c| SshTest {
            result: if c.timed_out {
                TestResult::Network(c.error_text())
            } else {
                ssh::parse_test(c.code, &c.stdout_text(), &c.stderr)
            },
            command: core::cmd_line("ssh", &args),
            ms: c.elapsed_ms,
        });
        Msg::SshTest(r)
    });
}

fn generate_key(cx: &mut Ctx, st: &mut State) {
    let Some(keygen) = st.keys.ok().and_then(|k| k.keygen.clone()) else {
        st.gen_form.error = "ssh-keygen is not installed".to_owned();
        return;
    };
    if st.gen_form.passphrase {
        return;
    }
    let path = PathBuf::from(core::from_msys(st.gen_form.path.trim()));
    match ssh::plan_generate(&keygen.to_string_lossy(), &path, &st.gen_form.email) {
        Ok(steps) => {
            st.gen_form.error.clear();
            confirm_plan(
                cx,
                Confirm {
                    title: "Generate a key without a passphrase?",
                    body: format!(
                        "Creates {p} (the private key) and {p}.pub. Without a passphrase, anyone who can read the private key file can use it: keep it on this machine only, and remove it from GitHub if the machine is lost.",
                        p = slash(&path)
                    ),
                    button: "Generate key",
                    danger: false,
                    label: "Generate SSH key",
                    steps,
                    extra: Vec::new(),
                    reload: Reload {
                        keys: true,
                        ..Default::default()
                    },
                },
            );
        }
        Err(e) => st.gen_form.error = e,
    }
}

/// The setup and its commands for the signing form (the view previews
/// them, the controller confirms them).
pub fn build_signing(
    form: &SigningForm,
    keys: Option<&KeysReport>,
    gpg: Option<&gpg::Report>,
) -> Result<(signing::Setup, Vec<Step>), String> {
    let setup = match form.format {
        Format::Ssh => {
            let k = keys
                .and_then(|r| {
                    r.keys
                        .iter()
                        .find(|k| Some(&k.file) == form.ssh_key.as_ref())
                })
                .ok_or("Choose an SSH key")?;
            let dir = ssh_dir().ok_or("No home folder for ~/.ssh")?;
            signing::ssh_setup(
                k,
                form.use_private,
                &form.email,
                form.scope,
                form.tags,
                &dir,
                git_home().as_deref(),
            )
        }
        Format::Gpg => {
            let rep = gpg.ok_or("The GPG keys are still loading")?;
            let k = rep
                .keys
                .iter()
                .find(|k| Some(&k.keyid) == form.gpg_key.as_ref())
                .ok_or("Choose a GPG key")?;
            signing::gpg_setup(k, rep.found.as_ref(), form.scope, form.tags)
        }
    };
    let steps = signing::plan(&setup)?;
    Ok((setup, steps))
}

fn submit_signing(cx: &mut Ctx, st: &mut State, g: &mut GlobalCx) {
    let form = st.sign.clone();
    g.settings.ext.identity.sign_tags = form.tags;
    if form.scope.editable() {
        g.settings.ext.identity.scope = form.scope;
    }
    let (setup, steps) = match build_signing(&form, st.keys.ok(), st.gpg.ok()) {
        Ok(x) => x,
        Err(e) => {
            st.sign.error = e;
            return;
        }
    };
    st.sign.error.clear();
    let body = format!(
        "From now on every new commit{} {} is signed with {}. {}",
        if form.tags { " and tag" } else { "" },
        match form.scope {
            Scope::Local => format!(
                "in {} (its own .git/config)",
                g.active_repo()
                    .map(|r| r.name.as_str())
                    .unwrap_or("this repository")
            ),
            _ => format!(
                "in every repository of yours (set in {})",
                place(form.scope, g)
            ),
        },
        setup.key,
        match form.format {
            Format::Ssh => {
                "Your e-mail and key go into the allowed-signers file, so git log --show-signature verifies your own signatures. GitHub shows Verified once the key is on your account as a signing key."
            }
            Format::Gpg => {
                "GitHub shows Verified once the public key is on your account and its e-mail matches the commit's."
            }
        }
    );
    confirm_plan(
        cx,
        Confirm {
            title: "Set up commit signing?",
            body,
            button: "Set up signing",
            danger: false,
            label: "Set up signing",
            steps,
            extra: Vec::new(),
            reload: Reload::CONFIG,
        },
    );
}

fn open_upload(st: &mut State, target: UploadTarget) {
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "this computer".to_owned());
    st.upload = Some(UploadForm {
        key_use: KeyUse::Signing,
        title: format!("gitvis on {host}"),
        target,
    });
}

fn submit_upload(cx: &mut Ctx, st: &mut State, g: &GlobalCx) {
    let Some(form) = st.upload.clone() else {
        return;
    };
    if form.title.trim().is_empty() {
        cx.toast(ToastKind::Error, "Give the key a title first.");
        return;
    }
    let login = st
        .account
        .ok()
        .and_then(|a| a.report.active().map(|a| a.login.clone()));
    let (steps, what, scope) = match &form.target {
        UploadTarget::Ssh(file) => (
            vec![Step::Gh(ssh::upload_args(file, form.key_use, &form.title))],
            format!(
                "{} as {} key",
                file.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                match form.key_use {
                    KeyUse::Signing => "a signing",
                    KeyUse::Authentication => "an authentication",
                }
            ),
            form.key_use.scope(),
        ),
        UploadTarget::Gpg(keyid) => {
            let Some(found) = st.gpg.ok().and_then(|r| r.found.clone()) else {
                cx.toast(ToastKind::Error, "gpg is not installed.");
                return;
            };
            (
                vec![Step::GpgUpload {
                    gpg: slash(&found.program),
                    key: keyid.clone(),
                    title: form.title.trim().to_owned(),
                }],
                format!("the GPG key {keyid}"),
                "admin:gpg_key",
            )
        }
    };
    let safe = if g.settings.safe_mode {
        " Safe mode is on: gitvis will refuse to send it until you turn safe mode off in Settings → Safety."
    } else {
        ""
    };
    confirm_plan(
        cx,
        Confirm {
            title: "Add the key to your GitHub account?",
            body: format!(
                "Uploads {what}, titled \"{}\", to {}. gh needs the {scope} scope for this.{safe}",
                form.title.trim(),
                match &login {
                    Some(l) => format!("the GitHub account {l}"),
                    None => "your GitHub account (the one gh is signed in with)".to_owned(),
                },
            ),
            button: "Add to GitHub",
            danger: false,
            label: "Add key to GitHub",
            steps,
            extra: Vec::new(),
            reload: Reload {
                keys: true,
                ..Default::default()
            },
        },
    );
}

pub fn on_msg(cx: &mut Ctx, st: &mut State, g: &mut GlobalCx, msg: Msg) {
    match msg {
        Msg::Config(r) => st.config.done(r),
        Msg::Identities(r) => match *r {
            Ok((snap, rep)) => {
                st.config.done(Ok(snap));
                // Rule indexes may have moved: an open edit form is closed.
                if let Some(f) = &st.id_form
                    && f.editing.is_some()
                    && st.identities.ok().map(|o| o.rules.len()) != Some(rep.rules.len())
                {
                    st.id_form = None;
                }
                st.identities.done(Ok(rep));
            }
            Err(e) => st.identities.done(Err(e)),
        },
        Msg::Keys(k) => {
            if st.gen_form.path.is_empty()
                && let Some(d) = &k.ssh_dir
            {
                st.gen_form.path = slash(&ssh::next_key_path(d));
            }
            if st.gen_form.email.is_empty() {
                st.gen_form.email = default_email(st);
            }
            if st.sign.ssh_key.is_none() {
                st.sign.ssh_key = k
                    .keys
                    .iter()
                    .find(|k| k.has_private)
                    .map(|k| k.file.clone());
            }
            st.keys.done(Ok(*k));
        }
        Msg::Gpg(r) => {
            if st.sign.gpg_key.is_none() {
                st.sign.gpg_key = r
                    .keys
                    .iter()
                    .find(|k| k.can_sign())
                    .map(|k| k.keyid.clone());
            }
            st.gpg.done(Ok(*r));
        }
        Msg::Account(a) => st.account.done(Ok(*a)),
        Msg::SshTest(r) => st.ssh_test.done(r),
        Msg::Check(a) => st.check.done(Ok(*a)),
        Msg::Changed(reload) => {
            st.busy = false;
            st.edit = None;
            st.raw = RawForm::default();
            st.upload = None;
            if reload.identities {
                st.id_form = None;
            }
            if reload.keys {
                st.gen_form.path.clear();
            }
            remember_repos(st, g);
            load_config(cx, st);
            if reload.identities {
                load_identities(cx, st);
            }
            if reload.keys {
                load_keys(cx, st);
                load_gpg(cx, st);
            }
            if reload.account {
                load_account(cx, st);
            }
        }
    }
}

/// Every frame; keep it cheap.
pub fn tick(_cx: &mut Ctx, _st: &mut State, _g: &mut GlobalCx) {}

fn show_section(section: Section) -> Vec<Action> {
    vec![
        Action::DevOps(DevOpsAction::SetSettingsSection(section)),
        Action::ShowPage(Page::Settings),
    ]
}

/// `GITVIS_SCRIPT` commands this area understands. Commands that would
/// change something stop at the confirmation.
pub fn script(cmd: &str, arg: &str) -> Option<Vec<Action>> {
    let arg = arg.trim();
    Some(match cmd {
        "config-tab" => {
            let mut v = show_section(Section::GitConfig);
            v.push(Act::SetConfigTab(ConfigTab::from_name(arg)?).into());
            v
        }
        // `config-key KEY [global|local]`: open the inline editor.
        "config-key" => {
            let mut words: Vec<&str> = arg.split_whitespace().collect();
            let scope = match words.last().copied() {
                Some("local") if words.len() > 1 => {
                    words.pop();
                    Scope::Local
                }
                Some("global") if words.len() > 1 => {
                    words.pop();
                    Scope::Global
                }
                _ => Scope::Global,
            };
            let key = (*words.first()?).to_owned();
            let common = config::COMMON
                .iter()
                .any(|c| config::norm_key(c.key) == config::norm_key(&key));
            let mut v = show_section(Section::GitConfig);
            v.push(
                Act::SetConfigTab(if common {
                    ConfigTab::Common
                } else {
                    ConfigTab::All
                })
                .into(),
            );
            v.push(
                Act::EditKey(EditForm {
                    key,
                    scope,
                    value: String::new(),
                })
                .into(),
            );
            v
        }
        "config-filter" => {
            let mut v = show_section(Section::GitConfig);
            v.push(Act::SetConfigTab(ConfigTab::All).into());
            v.push(Act::SetFilter(arg.to_owned()).into());
            v
        }
        // `config-edit KEY VALUE [global|local]`, `config-unset KEY [local]`.
        "config-edit" | "config-unset" => {
            let mut words: Vec<&str> = arg.split_whitespace().collect();
            let scope = match words.last().copied() {
                Some("global") => {
                    words.pop();
                    Scope::Global
                }
                Some("local") => {
                    words.pop();
                    Scope::Local
                }
                _ => Scope::Global,
            };
            let key = (*words.first()?).to_owned();
            let value = words[1..].join(" ");
            let mut v = show_section(Section::GitConfig);
            v.push(if cmd == "config-edit" {
                Act::SetConfig { scope, key, value }.into()
            } else {
                Act::UnsetConfig {
                    scope,
                    key,
                    value: (!value.is_empty()).then_some(value),
                }
                .into()
            });
            v
        }
        // `identity-form [FOLDER|NAME|EMAIL]`.
        "identity-form" => {
            let mut v = show_section(Section::Identities);
            v.push(Act::OpenIdentityForm(None).into());
            if !arg.is_empty() {
                let mut parts = arg.split('|').map(|s| s.trim().to_owned());
                v.push(
                    Act::FillIdentity {
                        folder: parts.next().unwrap_or_default(),
                        name: parts.next().unwrap_or_default(),
                        email: parts.next().unwrap_or_default(),
                    }
                    .into(),
                );
            }
            v
        }
        "identity-edit" => {
            let mut v = show_section(Section::Identities);
            v.push(Act::OpenIdentityForm(Some(arg.parse().ok()?)).into());
            v
        }
        "identity-add" => vec![Act::SubmitIdentity.into()],
        "check-path" => {
            let mut v = show_section(Section::Identities);
            v.push(Act::SetCheckPath(arg.to_owned()).into());
            v.push(Act::CheckPath.into());
            v
        }
        "ssh-test" => {
            let mut v = show_section(Section::Keys);
            v.push(Act::TestSsh.into());
            v
        }
        "signing-form" => {
            let f = match arg {
                "gpg" | "openpgp" => Format::Gpg,
                _ => Format::Ssh,
            };
            let mut v = show_section(Section::Keys);
            v.push(Act::SigningKind(f).into());
            v
        }
        "signing-apply" => vec![Act::SubmitSigning.into()],
        "key-generate" => vec![Act::GenerateKey.into()],
        "upload-key" => {
            let mut v = show_section(Section::Keys);
            v.push(Act::OpenUploadSsh(arg.parse().ok()?).into());
            v
        }
        "upload-submit" => vec![Act::SubmitUpload.into()],
        // `gh-switch LOGIN` or `gh-switch HOST LOGIN`.
        "gh-switch" => {
            let mut v = show_section(Section::GitHubAccount);
            let (host, login) = match arg.split_once(' ') {
                Some((h, l)) => (h.to_owned(), l.trim().to_owned()),
                None => ("github.com".to_owned(), arg.to_owned()),
            };
            if login.is_empty() {
                return None;
            }
            v.push(Act::SwitchAccount { host, login }.into());
            v
        }
        "gh-setup-git" => vec![Act::SetupGit.into()],
        "id-scroll" => vec![Act::Scroll(arg.parse().ok()?).into()],
        _ => return None,
    })
}

/// A Settings section became visible (check `section`; most are not yours).
pub fn on_section_shown(cx: &mut Ctx, st: &mut State, g: &mut GlobalCx, section: Section) {
    if !matches!(
        section,
        Section::GitConfig | Section::Identities | Section::Keys | Section::GitHubAccount
    ) {
        return;
    }
    remember_repos(st, g);
    if st.check_path.is_empty() {
        st.check_path = g.settings.ext.identity.check_path.clone();
    }
    match section {
        // The account is network: once, then on Refresh.
        Section::GitHubAccount => {
            load_config(cx, st);
            if st.account.data.is_none() {
                load_account(cx, st);
            }
        }
        s => reload_section(cx, st, s),
    }
}

/// Git config, Identities, Keys and signing, GitHub account sections.
pub fn settings_section(
    ui: &mut egui::Ui,
    st: &mut State,
    settings: &mut Settings,
    repo: Option<&RepoState>,
    section: Section,
    actions: &mut Vec<Action>,
) {
    match section {
        Section::GitConfig => config_view::show(ui, st, settings, repo, actions),
        Section::Identities => identities_view::show(ui, st, settings, repo, actions),
        Section::Keys => keys_view::show(ui, st, settings, repo, actions),
        Section::GitHubAccount => account_view::show(ui, st, settings, actions),
        _ => {}
    }
}

#[allow(dead_code)]
fn _unused(_: &GlobalView) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devops::DevOpsAction;

    fn acts(cmd: &str, arg: &str) -> Vec<Act> {
        script(cmd, arg)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|a| match a {
                Action::DevOps(DevOpsAction::Identity(a)) => Some(a),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn script_commands() {
        assert!(matches!(
            acts("config-edit", "pull.ff only local").as_slice(),
            [Act::SetConfig { scope: Scope::Local, key, value }] if key == "pull.ff" && value == "only"
        ));
        assert!(matches!(
            acts("config-edit", "core.editor code --wait").as_slice(),
            [Act::SetConfig { scope: Scope::Global, value, .. }] if value == "code --wait"
        ));
        assert!(matches!(
            acts("config-unset", "core.editor").as_slice(),
            [Act::UnsetConfig { value: None, .. }]
        ));
        assert!(matches!(
            acts("identity-form", "C:/work|Ada|ada@corp.example").as_slice(),
            [Act::OpenIdentityForm(None), Act::FillIdentity { email, .. }] if email == "ada@corp.example"
        ));
        assert!(matches!(
            acts("signing-form", "gpg").as_slice(),
            [Act::SigningKind(Format::Gpg)]
        ));
        assert!(matches!(
            acts("config-filter", "github").as_slice(),
            [Act::SetConfigTab(ConfigTab::All), Act::SetFilter(f)] if f == "github"
        ));
        assert!(script("config-tab", "nope").is_none());
        assert!(script("config-edit", "").is_none());
        assert!(script("gh-switch", "").is_none());
        assert!(script("not-mine", "").is_none());
        // The section is shown first.
        let v = script("ssh-test", "").unwrap();
        assert!(matches!(
            v[0],
            Action::DevOps(DevOpsAction::SetSettingsSection(Section::Keys))
        ));
    }

    #[test]
    fn prefs_round_trip() {
        let p = Prefs {
            scope: Scope::Local,
            collapsed: vec!["remote.origin".into()],
            config_tab: "all".into(),
            ..Default::default()
        };
        let back: Prefs = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back.scope, Scope::Local);
        assert_eq!(back.tab(), ConfigTab::All);
        assert_eq!(back.collapsed, ["remote.origin"]);
        let empty: Prefs = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.scope, Scope::Global);
        assert_eq!(empty.tab(), ConfigTab::Common);
        assert!(empty.sign_tags);
    }

    #[test]
    fn loaded_state() {
        let mut l: Loaded<u32> = Loaded::default();
        assert!(l.start());
        assert!(!l.start(), "one load at a time");
        l.done(Ok(3));
        assert_eq!(l.ok(), Some(&3));
        assert!(!l.loading && l.at.is_some());
    }
}
