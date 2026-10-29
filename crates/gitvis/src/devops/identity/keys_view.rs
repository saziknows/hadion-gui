//! Settings → Keys and signing: what signing is for, the current signing
//! setup, the setup form, SSH keys (fingerprints, agent, the GitHub test,
//! a new key), GPG keys, and uploading a key to GitHub.

use std::path::PathBuf;

use egui::{RichText, Ui};
use gitvis_core::devops::identity::account;
use gitvis_core::devops::identity::config::Snapshot;
use gitvis_core::devops::identity::gpg::{self, Key};
use gitvis_core::devops::identity::signing::Format;
use gitvis_core::devops::identity::ssh::{self, Agent, KeyUse, PubKey, TestResult};
use gitvis_core::devops::identity::{
    Scope, display_steps, expand_tilde, git_home, slash, ssh_home, tilde,
};

use super::widgets as w;
use super::{
    AccountData, Act, GenerateForm, KeysReport, Loaded, SigningForm, SshTest, State, UploadForm,
    UploadTarget, build_signing,
};
use crate::devops::settings_page::Section;
use crate::format;
use crate::state::{Action, RepoState, Settings};
use crate::theme::{pal, size, space};
use crate::views::form::{self, InputOpts};
use crate::views::icons::{Icon, IconButton};
use crate::views::kit::{self, Tone};

pub fn show(
    ui: &mut Ui,
    st: &mut State,
    settings: &mut Settings,
    repo: Option<&RepoState>,
    actions: &mut Vec<Action>,
) {
    let safe = settings.safe_mode;
    let State {
        keys,
        gpg,
        config,
        ssh_test,
        gen_form,
        sign,
        upload,
        account,
        busy,
        scroll,
        ..
    } = st;
    let busy = *busy;
    if w::header(
        ui,
        "Keys and signing",
        "SSH and GPG keys on this computer, commit signing, adding a key to GitHub",
        keys.loading || gpg.loading,
        keys.at,
        |_| {},
    ) {
        actions.push(Act::Refresh(Section::Keys).into());
    }
    let repo_name = repo.map(|r| r.name.clone()).unwrap_or_default();
    w::page(ui, "identity_keys", scroll, |ui| {
        kit::callout_with(
            ui,
            Tone::Accent,
            "What signing is for",
            "A signature proves who made a commit: anyone can write your name and e-mail into a commit, only you hold your key. GitHub shows:",
            |ui| {
                ui.add_space(2.0);
                let small = |t: &str| RichText::new(t).size(size::SMALL);
                kit::kv(ui, 92.0, |kv| {
                    kv.text(
                        "Verified",
                        small("signed with a key on the committer's GitHub account (an SSH key added as a signing key)"),
                    );
                    kv.text(
                        "Unverified",
                        small("signed, but the key is not on the committer's account"),
                    );
                    kv.text("Nothing", small("unsigned commits"));
                });
            },
        );
        status(ui, config.ok(), keys.ok(), gpg.ok());
        setup_form(
            ui,
            sign,
            keys.ok(),
            gpg.ok(),
            config.ok(),
            &repo_name,
            busy,
            actions,
        );

        let up = Uploads {
            form: upload,
            account: account.ok(),
            safe,
            busy,
        };
        let mut up = up;
        ssh_keys(ui, keys, &mut up, config.ok(), actions);
        if let Some(k) = keys.ok() {
            agent(ui, k, config.ok(), actions);
            github_test(ui, k, ssh_test, actions);
            generate(ui, gen_form, k, busy, actions);
        }
        gpg_keys(ui, gpg, &mut up, config.ok(), actions);
    });
}

/// The upload form and what it needs to know.
struct Uploads<'a> {
    form: &'a mut Option<UploadForm>,
    account: Option<&'a AccountData>,
    safe: bool,
    busy: bool,
}

fn eff<'a>(
    cfg: Option<&'a Snapshot>,
    key: &str,
) -> Option<&'a gitvis_core::devops::identity::config::Entry> {
    cfg.and_then(|s| s.effective(key))
}

fn status(
    ui: &mut Ui,
    cfg: Option<&Snapshot>,
    keys: Option<&KeysReport>,
    gpg: Option<&gpg::Report>,
) {
    kit::section(ui, "Signing now", None);
    let Some(cfg) = cfg else {
        kit::loading(ui, "Reading git config");
        return;
    };
    let home = git_home();
    let format = eff(Some(cfg), "gpg.format");
    let key = eff(Some(cfg), "user.signingkey");
    let commits = eff(Some(cfg), "commit.gpgsign").is_some_and(|e| e.value_text() == "true");
    let tags = eff(Some(cfg), "tag.gpgsign").is_some_and(|e| e.value_text() == "true");
    let fmt = format
        .map(|e| e.value_text().to_owned())
        .unwrap_or_else(|| "openpgp".to_owned());
    let matched: Option<String> = key.and_then(|k| {
        if fmt == "ssh" {
            keys.and_then(|r| {
                r.keys
                    .iter()
                    .find(|p| p.is(k.value_text(), ssh_home().as_deref()))
            })
            .map(|p| format!("{} ({}, {})", p.name(), p.kind(), p.fingerprint.hash))
        } else {
            gpg.and_then(|r| {
                r.keys.iter().find(|g| {
                    g.keyid
                        .eq_ignore_ascii_case(k.value_text().trim_end_matches('!'))
                        || g.fingerprint.eq_ignore_ascii_case(k.value_text())
                })
            })
            .map(|g| format!("{} ({})", g.primary_uid(), g.algo))
        }
    });
    form::card(
        ui,
        if commits && key.is_some() {
            Tone::Success
        } else {
            Tone::Neutral
        },
        |ui| {
            w::badges(
                ui,
                &[
                    if commits {
                        ("Commits are signed".to_owned(), Tone::Success)
                    } else {
                        ("Commits are not signed".to_owned(), Tone::Neutral)
                    },
                    if tags {
                        ("Tags are signed".to_owned(), Tone::Success)
                    } else {
                        ("Tags are not signed".to_owned(), Tone::Neutral)
                    },
                ],
            );
            ui.add_space(4.0);
            let warn = pal(ui).warning;
            kit::kv(ui, 150.0, |kv| {
                let row = |kv: &mut kit::Kv,
                           label: &str,
                           e: Option<&gitvis_core::devops::identity::config::Entry>,
                           unset: &str| match e {
                    Some(e) => kv.row(label, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(kit::mono(e.value_text()).color(pal(ui).text));
                            w::scope_badge(ui, e.scope);
                        });
                    }),
                    None => kv.muted(label, unset),
                };
                row(
                    kv,
                    "Format (gpg.format)",
                    format,
                    "not set: openpgp (GnuPG)",
                );
                row(kv, "Key (user.signingkey)", key, "not set");
                if key.is_some() {
                    match &matched {
                        Some(m) => kv.text("That is", m.clone()),
                        None => kv.text(
                            "That is",
                            RichText::new(if fmt == "ssh" {
                                "No key in ~/.ssh matches: signing will fail"
                            } else {
                                "No GPG secret key with that id was listed"
                            })
                            .color(warn),
                        ),
                    }
                }
                row(
                    kv,
                    "commit.gpgsign",
                    eff(Some(cfg), "commit.gpgsign"),
                    "not set: false",
                );
                row(
                    kv,
                    "tag.gpgSign",
                    eff(Some(cfg), "tag.gpgsign"),
                    "not set: false",
                );
                if fmt == "ssh" {
                    match eff(Some(cfg), "gpg.ssh.allowedsignersfile") {
                    Some(e) => {
                        let p = expand_tilde(e.value_text(), home.as_deref());
                        kv.row("Allowed signers", |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(kit::mono(e.value_text()).color(pal(ui).text));
                                if !p.is_file() {
                                    kit::badge(ui, "File missing", Tone::Warning);
                                }
                            });
                        });
                    }
                    None => kv.muted(
                        "Allowed signers",
                        "not set: git cannot verify SSH signatures (git log --show-signature says \"No principal matched\")",
                    ),
                }
                }
            });
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn setup_form(
    ui: &mut Ui,
    f: &mut SigningForm,
    keys: Option<&KeysReport>,
    gpg: Option<&gpg::Report>,
    cfg: Option<&Snapshot>,
    repo_name: &str,
    busy: bool,
    actions: &mut Vec<Action>,
) {
    kit::section(ui, "Set up signing", None);
    if f.email.is_empty()
        && let Some(e) = eff(cfg, "user.email")
    {
        f.email = e.value_text().to_owned();
    }
    form::card(ui, Tone::Neutral, |ui| {
        w::label(ui, "Sign with");
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            if form::radio(ui, f.format == Format::Ssh, "An SSH key") && f.format != Format::Ssh {
                actions.push(Act::SigningKind(Format::Ssh).into());
            }
            if form::radio(ui, f.format == Format::Gpg, "A GPG key") && f.format != Format::Gpg {
                actions.push(Act::SigningKind(Format::Gpg).into());
            }
        });
        ui.add_space(4.0);
        match f.format {
            Format::Ssh => {
                let list: Vec<&PubKey> = keys.map(|k| k.keys.iter().collect()).unwrap_or_default();
                if list.is_empty() {
                    kit::muted(ui, "No SSH key in ~/.ssh: generate one below first.");
                } else {
                    let opts: Vec<(Option<PathBuf>, String)> = list
                        .iter()
                        .map(|k| {
                            (
                                Some(k.file.clone()),
                                format!(
                                    "{}  ·  {}{}",
                                    k.name(),
                                    k.kind(),
                                    if k.comment.is_empty() {
                                        String::new()
                                    } else {
                                        format!("  ·  {}", k.comment)
                                    }
                                ),
                            )
                        })
                        .collect();
                    form::field(ui, "Key", None, |ui| {
                        form::select(ui, "identity_sign_ssh", &mut f.ssh_key, &opts, 420.0);
                    });
                    if form::radio(
                        ui,
                        f.use_private,
                        "Point user.signingkey at the private key file: works without ssh-agent",
                    ) {
                        f.use_private = true;
                    }
                    if form::radio(
                        ui,
                        !f.use_private,
                        "Point it at the public key: the private key must be loaded in ssh-agent",
                    ) {
                        f.use_private = false;
                    }
                    ui.add_space(4.0);
                    form::field(
                        ui,
                        "Your e-mail",
                        Some(
                            "Goes with the key into the allowed-signers file, so git verifies your own signatures.",
                        ),
                        |ui| {
                            form::input(
                                ui,
                                "identity_sign_email",
                                &mut f.email,
                                InputOpts::new("you@example.com").mono().width(420.0),
                            );
                        },
                    );
                }
            }
            Format::Gpg => match gpg {
                None => kit::loading(ui, "Listing GPG keys"),
                Some(r) if r.found.is_none() => {
                    kit::muted(ui, "gpg is not installed: see GPG keys below.");
                }
                Some(r) => {
                    let now = format::now_secs();
                    let usable: Vec<&Key> = r
                        .keys
                        .iter()
                        .filter(|k| k.can_sign() && !k.expired(now) && !k.revoked())
                        .collect();
                    if usable.is_empty() {
                        kit::muted(
                            ui,
                            "No GPG key that can sign (and is not expired or revoked).",
                        );
                    } else {
                        let opts: Vec<(Option<String>, String)> = usable
                            .iter()
                            .map(|k| {
                                (
                                    Some(k.keyid.clone()),
                                    format!("{}  ·  {}", k.keyid, k.primary_uid()),
                                )
                            })
                            .collect();
                        form::field(ui, "Key", None, |ui| {
                            form::select(ui, "identity_sign_gpg", &mut f.gpg_key, &opts, 480.0);
                        });
                    }
                }
            },
        }
        ui.add_space(4.0);
        w::label(ui, "Where");
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            if form::radio(
                ui,
                f.scope == Scope::Global,
                "Global: every repository of yours",
            ) {
                f.scope = Scope::Global;
            }
            let local = if repo_name.is_empty() {
                "This repository only".to_owned()
            } else {
                format!("Only this repository ({repo_name})")
            };
            if form::radio(ui, f.scope == Scope::Local, &local) {
                f.scope = Scope::Local;
            }
        });
        ui.add_space(2.0);
        form::checkbox(ui, &mut f.tags, "Sign tags too (tag.gpgSign)");
        ui.add_space(4.0);
        let built = build_signing(f, keys, gpg);
        match &built {
            Ok((_, steps)) => {
                w::label(ui, "What gitvis will run");
                kit::code_block(ui, &display_steps(steps).join("\n"), actions);
            }
            Err(e) => {
                kit::secondary(ui, e);
            }
        }
        form::error(ui, &f.error);
        form::actions_row(ui, |ui| {
            if form::primary(ui, "Set up signing…", built.is_ok() && !busy).clicked() {
                actions.push(Act::SubmitSigning.into());
            }
        });
    });
}

fn ssh_keys(
    ui: &mut Ui,
    keys: &Loaded<KeysReport>,
    up: &mut Uploads,
    cfg: Option<&Snapshot>,
    actions: &mut Vec<Action>,
) {
    let home = ssh_home();
    let dir_text = keys
        .ok()
        .and_then(|k| k.ssh_dir.as_deref())
        .map(|d| tilde(d, home.as_deref()))
        .unwrap_or_default();
    kit::section_with(ui, "SSH keys", keys.ok().map(|k| k.keys.len()), |ui| {
        if !dir_text.is_empty() {
            ui.label(kit::mono(dir_text.clone()).color(pal(ui).text_3));
        }
    });
    let k = match &keys.data {
        None => {
            kit::loading(ui, "Reading ~/.ssh");
            return;
        }
        Some(Err(e)) => {
            if w::error_card(ui, "Could not read the SSH keys", e, "Try again") {
                actions.push(Act::Refresh(Section::Keys).into());
            }
            return;
        }
        Some(Ok(k)) => k,
    };
    if k.keygen.is_none()
        && w::missing_tool(
            ui,
            "ssh-keygen",
            "ssh-keygen",
            "It prints fingerprints and creates keys; the fingerprints below were computed by gitvis instead.",
            "Windows: Settings → System → Optional features → OpenSSH Client, or install Git for Windows, which ships it.",
        )
    {
        actions.push(Act::Refresh(Section::Keys).into());
    }
    if !k.dir_exists {
        kit::callout(
            ui,
            Tone::Neutral,
            "No ~/.ssh folder yet",
            &format!(
                "{} does not exist: you have no SSH keys on this computer. Generate one below.",
                dir_text
            ),
        );
        return;
    }
    if k.keys.is_empty() {
        kit::callout(
            ui,
            Tone::Neutral,
            "No SSH keys",
            &format!("There is no *.pub file in {dir_text}. Generate a key below."),
        );
        return;
    }
    let signing = eff(cfg, "user.signingkey").map(|e| e.value_text().to_owned());
    let ssh_format = eff(cfg, "gpg.format").is_some_and(|e| e.value_text() == "ssh");
    let in_agent: Vec<String> = match &k.agent {
        Some(Agent::Keys(v)) => v.iter().map(|f| f.hash.clone()).collect(),
        _ => Vec::new(),
    };
    for (i, key) in k.keys.iter().enumerate() {
        let p = *pal(ui);
        form::card(ui, Tone::Neutral, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.label(kit::mono(key.name()).size(size::BODY).color(p.text_strong));
                let kind = match key.fingerprint.bits {
                    Some(b) => format!("{} {b}", key.kind()),
                    None => key.kind(),
                };
                kit::badge(ui, &kind, Tone::Neutral);
                if !key.has_private {
                    kit::badge(ui, "Private key missing", Tone::Warning);
                }
                if in_agent.contains(&key.fingerprint.hash) {
                    kit::badge(ui, "Loaded in ssh-agent", Tone::Success);
                }
                if ssh_format
                    && signing
                        .as_deref()
                        .is_some_and(|s| key.is(s, home.as_deref()))
                {
                    kit::badge(ui, "user.signingkey", Tone::Accent);
                }
            });
            ui.add_space(2.0);
            kit::kv(ui, 110.0, |kv| {
                kv.row("Fingerprint", |ui| {
                    w::copyable(ui, &key.fingerprint.hash, actions)
                });
                if key.comment.is_empty() {
                    kv.muted("Comment", "none");
                } else {
                    kv.text("Comment", key.comment.clone());
                }
                kv.muted(
                    "Files",
                    &format!(
                        "{}{}",
                        tilde(&key.file, home.as_deref()),
                        if key.has_private {
                            format!(" and {}", tilde(&key.private, home.as_deref()))
                        } else {
                            " (no private key next to it)".to_owned()
                        }
                    ),
                );
                if let Some(e) = &key.error {
                    kv.text("ssh-keygen", RichText::new(e.as_str()).color(p.danger));
                }
            });
            w::buttons(ui, |ui| {
                if key.has_private
                    && IconButton::new(Icon::Key, "Use for signing")
                        .compact()
                        .show(ui)
                        .clicked()
                {
                    actions.push(Act::SigningSsh(i).into());
                }
                if IconButton::new(Icon::Cloud, "Add to GitHub…")
                    .compact()
                    .show(ui)
                    .clicked()
                {
                    actions.push(Act::OpenUpload(UploadTarget::Ssh(key.file.clone())).into());
                }
                if IconButton::new(Icon::Copy, "Copy public key")
                    .compact()
                    .show(ui)
                    .clicked()
                {
                    actions.push(Action::Copy(key.line()));
                }
            });
            if up
                .form
                .as_ref()
                .is_some_and(|f| f.target == UploadTarget::Ssh(key.file.clone()))
            {
                upload_form(ui, up, actions);
            }
        });
        ui.add_space(space::S);
    }
}

fn agent(ui: &mut Ui, k: &KeysReport, cfg: Option<&Snapshot>, actions: &mut Vec<Action>) {
    kit::section(ui, "SSH agent", None);
    let home = ssh_home();
    match &k.agent {
        None => {
            w::missing_tool(
                ui,
                "ssh-add",
                "ssh-add",
                "It lists the keys the agent holds.",
                "Windows: Settings → System → Optional features → OpenSSH Client, or Git for Windows.",
            );
        }
        Some(Agent::Keys(v)) => {
            kit::body(
                ui,
                &format!(
                    "Running, with {} loaded:",
                    format::plural(v.len(), "key", "keys")
                ),
            );
            for f in v {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.label(kit::mono(&f.hash).color(pal(ui).text));
                    ui.label(
                        RichText::new(format!(
                            "{} · {}",
                            f.kind,
                            if f.comment.is_empty() {
                                "no comment"
                            } else {
                                &f.comment
                            }
                        ))
                        .size(size::SMALL)
                        .color(pal(ui).text_2),
                    );
                });
            }
        }
        Some(Agent::Empty) => {
            kit::body(
                ui,
                "Running, with no key loaded. ssh-add ~/.ssh/id_ed25519 loads one; it asks for the key's passphrase once.",
            );
        }
        Some(Agent::NotRunning(out)) => {
            kit::callout_with(
                ui,
                Tone::Neutral,
                "No ssh-agent is running",
                "Common on Windows, and usually fine: a key without a passphrase works without an agent. With a passphrase, ssh asks for it on every connection, and gitvis, which has no terminal, cannot answer.",
                |ui| {
                    ui.add_space(4.0);
                    w::label(
                        ui,
                        "Start Windows' agent (a service, off by default) in an administrator PowerShell",
                    );
                    kit::code_block(
                        ui,
                        "Get-Service ssh-agent | Set-Service -StartupType Automatic\nStart-Service ssh-agent\nssh-add $env:USERPROFILE\\.ssh\\id_ed25519",
                        actions,
                    );
                    ui.add_space(2.0);
                    w::label(
                        ui,
                        "Git for Windows' own ssh does not use that agent; to make git use Windows' ssh",
                    );
                    kit::code_block(
                        ui,
                        "git config --global core.sshCommand C:/Windows/System32/OpenSSH/ssh.exe",
                        actions,
                    );
                },
            );
            ui.add_space(2.0);
            kit::secondary(
                ui,
                &format!("ssh-add -l said: {}", out.lines().next().unwrap_or("")),
            );
        }
        Some(Agent::Error(e)) => {
            kit::callout(ui, Tone::Warning, "ssh-add -l failed", e);
        }
    }
    ui.add_space(4.0);
    kit::kv(ui, 150.0, |kv| {
        match &k.ssh {
            Some(p) => kv.text("ssh", kit::mono(tilde(p, home.as_deref()))),
            None => kv.muted("ssh", "not found on PATH"),
        }
        match &k.ssh_add {
            Some(p) => kv.text("ssh-add", kit::mono(tilde(p, home.as_deref()))),
            None => kv.muted("ssh-add", "not found on PATH"),
        }
        match eff(cfg, "core.sshcommand") {
            Some(e) => kv.text("git uses (core.sshCommand)", kit::mono(e.value_text())),
            None => kv.muted("git uses", "its own ssh (core.sshCommand is not set)"),
        }
    });
}

fn github_test(ui: &mut Ui, k: &KeysReport, t: &Loaded<SshTest>, actions: &mut Vec<Action>) {
    kit::section(ui, "Test GitHub access", None);
    kit::secondary(
        ui,
        "Connects to github.com over SSH with your keys (ssh -T); GitHub answers with the account a key belongs to. Nothing changes on GitHub.",
    );
    if k.github_known == Some(false) {
        kit::secondary(
            ui,
            "github.com is not in ~/.ssh/known_hosts yet: on the first connection ssh adds its host key there (StrictHostKeyChecking=accept-new).",
        );
    }
    ui.add_space(2.0);
    if t.loading {
        kit::loading(ui, "Connecting to git@github.com…");
    } else if IconButton::new(
        Icon::Play,
        if t.data.is_some() {
            "Test again"
        } else {
            "Test"
        },
    )
    .compact()
    .enabled(k.ssh.is_some())
    .show(ui)
    .clicked()
    {
        actions.push(Act::TestSsh.into());
    }
    match &t.data {
        None => {}
        Some(Err(e)) => kit::callout(ui, Tone::Danger, "The test did not run", e),
        Some(Ok(r)) => {
            ui.add_space(2.0);
            match &r.result {
                TestResult::Authenticated(u) => kit::callout(
                    ui,
                    Tone::Success,
                    &format!("GitHub knows this key: {u}"),
                    &format!("git@github.com: remotes work for the repositories {u} can access."),
                ),
                TestResult::Denied(out) => kit::callout(
                    ui,
                    Tone::Danger,
                    "GitHub does not know any of these keys",
                    &format!(
                        "Add a key to GitHub as an authentication key (Add to GitHub above), or load the right key in ssh-agent.\n\n{out}"
                    ),
                ),
                TestResult::HostKey(out) => kit::callout(
                    ui,
                    Tone::Danger,
                    "github.com's host key could not be checked",
                    &format!(
                        "The key in ~/.ssh/known_hosts differs from the one github.com sent, or it is missing. Compare with https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/githubs-ssh-key-fingerprints before changing known_hosts.\n\n{out}"
                    ),
                ),
                TestResult::Network(out) => kit::callout(
                    ui,
                    Tone::Warning,
                    "Could not reach github.com on port 22",
                    &format!(
                        "Some networks block port 22. GitHub also answers SSH on port 443: in ~/.ssh/config, Host github.com, Hostname ssh.github.com, Port 443.\n\n{out}"
                    ),
                ),
                TestResult::Failed(out) => kit::callout(ui, Tone::Danger, "ssh failed", out),
            }
            kit::muted(ui, &format!("{}  ·  {} ms", r.command, r.ms));
        }
    }
}

fn generate(
    ui: &mut Ui,
    g: &mut GenerateForm,
    k: &KeysReport,
    busy: bool,
    actions: &mut Vec<Action>,
) {
    kit::section(ui, "Generate a new key", None);
    form::card(ui, Tone::Neutral, |ui| {
        form::field(
            ui,
            "E-mail",
            Some("Goes into the key's comment, to recognise it later."),
            |ui| {
                form::input(
                    ui,
                    "identity_gen_email",
                    &mut g.email,
                    InputOpts::new("you@example.com").mono().width(420.0),
                );
            },
        );
        form::field(
            ui,
            "File",
            Some("The private key; the public one gets .pub after it."),
            |ui| {
                form::input(
                    ui,
                    "identity_gen_path",
                    &mut g.path,
                    InputOpts::new("C:/Users/you/.ssh/id_ed25519").mono(),
                );
            },
        );
        if form::radio(
            ui,
            !g.passphrase,
            "No passphrase: gitvis runs ssh-keygen now",
        ) {
            g.passphrase = false;
        }
        if form::radio(
            ui,
            g.passphrase,
            "With a passphrase: run the command in a terminal",
        ) {
            g.passphrase = true;
        }
        ui.add_space(4.0);
        let path = PathBuf::from(gitvis_core::devops::identity::from_msys(g.path.trim()));
        if g.passphrase {
            kit::secondary(
                ui,
                "ssh-keygen asks for the passphrase twice in the terminal; gitvis never sees it, and it never goes on a command line. Press Refresh afterwards.",
            );
            kit::code_block(ui, &ssh::terminal_command(&path, &g.email), actions);
        } else {
            kit::callout(
                ui,
                Tone::Warning,
                "Without a passphrase",
                "Anyone who can read the private key file can use the key. Keep it on this computer only, and remove it from GitHub if the computer is lost.",
            );
            if let Some(kg) = &k.keygen {
                match ssh::plan_generate(&kg.to_string_lossy(), &path, &g.email) {
                    Ok(steps) => kit::code_block(ui, &display_steps(&steps).join("\n"), actions),
                    Err(e) => {
                        kit::secondary(ui, &e);
                    }
                }
            }
            form::error(ui, &g.error);
            form::actions_row(ui, |ui| {
                if form::primary(ui, "Generate key…", k.keygen.is_some() && !busy).clicked() {
                    actions.push(Act::GenerateKey.into());
                }
            });
        }
    });
}

fn gpg_keys(
    ui: &mut Ui,
    gpg: &Loaded<gpg::Report>,
    up: &mut Uploads,
    cfg: Option<&Snapshot>,
    actions: &mut Vec<Action>,
) {
    kit::section(ui, "GPG keys", gpg.ok().map(|r| r.keys.len()));
    let r = match &gpg.data {
        None => {
            kit::loading(ui, "Looking for gpg");
            return;
        }
        Some(Err(e)) => {
            if w::error_card(ui, "Could not read the GPG keys", e, "Try again") {
                actions.push(Act::Refresh(Section::Keys).into());
            }
            return;
        }
        Some(Ok(r)) => r,
    };
    let Some(found) = &r.found else {
        if w::missing_tool(
            ui,
            "GnuPG",
            "gpg",
            "Listing GPG keys and signing with one need it; SSH signing does not.",
            "winget install GnuPG.Gpg4win. Git for Windows also ships gpg in C:/Program Files/Git/usr/bin; gitvis finds that one when git is on PATH.",
        ) {
            actions.push(Act::Refresh(Section::Keys).into());
        }
        return;
    };
    kit::kv(ui, 110.0, |kv| {
        kv.text("gpg", kit::mono(slash(&found.program)));
        kv.muted("Found through", found.how);
        if let Some(v) = &r.version {
            kv.text("Version", v.clone());
        }
        match &r.home {
            Some(h) if r.home_exists => kv.text("Keyring", kit::mono(h.as_str())),
            Some(h) => kv.muted("Keyring", &format!("{h} does not exist yet: no keys")),
            None => kv.muted("Keyring", "unknown"),
        }
    });
    if found.how == "Git for Windows" {
        kit::secondary(
            ui,
            "This is the gpg that Git for Windows ships; Gpg4win keeps its keys in another keyring (%APPDATA%/gnupg).",
        );
    }
    if let Some(e) = &r.error {
        if w::error_card(ui, "gpg failed", e, "Try again") {
            actions.push(Act::Refresh(Section::Keys).into());
        }
        return;
    }
    if r.keys.is_empty() {
        kit::callout(
            ui,
            Tone::Neutral,
            "No GPG secret keys",
            "gpg --full-generate-key in a terminal creates one (it asks for a passphrase there).",
        );
        return;
    }
    let now = format::now_secs();
    let signing =
        eff(cfg, "user.signingkey").map(|e| e.value_text().trim_end_matches('!').to_owned());
    let gpg_format = eff(cfg, "gpg.format").is_none_or(|e| e.value_text() == "openpgp");
    for (i, k) in r.keys.iter().enumerate() {
        form::card(
            ui,
            if k.expired(now) || k.revoked() {
                Tone::Danger
            } else {
                Tone::Neutral
            },
            |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    kit::subtitle(ui, k.primary_uid());
                    kit::badge(ui, &k.algo, Tone::Neutral);
                    if k.revoked() {
                        kit::badge(ui, "Revoked", Tone::Danger);
                    } else if k.expired(now) {
                        kit::badge(ui, "Expired", Tone::Danger);
                    } else if let Some(e) = k.expires
                        && e - now < 30 * 86_400
                    {
                        kit::badge(
                            ui,
                            &format!("Expires in {} days", (e - now) / 86_400),
                            Tone::Warning,
                        );
                    }
                    if !k.can_sign() {
                        kit::badge(ui, "Cannot sign", Tone::Neutral);
                    }
                    if gpg_format
                        && signing.as_deref().is_some_and(|s| {
                            s.eq_ignore_ascii_case(&k.keyid)
                                || s.eq_ignore_ascii_case(&k.fingerprint)
                        })
                    {
                        kit::badge(ui, "user.signingkey", Tone::Accent);
                    }
                });
                ui.add_space(2.0);
                kit::kv(ui, 110.0, |kv| {
                    kv.row("Key id", |ui| w::copyable(ui, &k.keyid, actions));
                    if !k.fingerprint.is_empty() {
                        kv.text("Fingerprint", kit::mono(k.fingerprint.as_str()));
                    }
                    if let Some(c) = k.created {
                        kv.text("Created", format::absolute(c));
                    }
                    match k.expires {
                        Some(e) => kv.text("Expires", format::absolute(e)),
                        None => kv.muted("Expires", "never"),
                    }
                    kv.text("Can", k.capabilities().join(", "));
                    if k.uids.len() > 1 {
                        kv.text(
                            "Other user ids",
                            k.uids[1..]
                                .iter()
                                .map(|u| u.text.clone())
                                .collect::<Vec<_>>()
                                .join("\n"),
                        );
                    }
                    if !k.subkeys.is_empty() {
                        kv.muted(
                            "Subkeys",
                            &k.subkeys
                                .iter()
                                .map(|s| format!("{} ({}, {})", s.keyid, s.algo, s.caps))
                                .collect::<Vec<_>>()
                                .join(", "),
                        );
                    }
                });
                w::buttons(ui, |ui| {
                    if k.can_sign()
                        && !k.expired(now)
                        && IconButton::new(Icon::Key, "Use for signing")
                            .compact()
                            .show(ui)
                            .clicked()
                    {
                        actions.push(Act::SigningGpg(i).into());
                    }
                    if IconButton::new(Icon::Cloud, "Add to GitHub…")
                        .compact()
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Act::OpenUpload(UploadTarget::Gpg(k.keyid.clone())).into());
                    }
                    if IconButton::new(Icon::Copy, "Copy key id")
                        .compact()
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Action::Copy(k.keyid.clone()));
                    }
                });
                if up
                    .form
                    .as_ref()
                    .is_some_and(|f| f.target == UploadTarget::Gpg(k.keyid.clone()))
                {
                    upload_form(ui, up, actions);
                }
            },
        );
        ui.add_space(space::S);
    }
}

fn upload_form(ui: &mut Ui, up: &mut Uploads, actions: &mut Vec<Action>) {
    let Some(f) = up.form.as_mut() else {
        return;
    };
    ui.add_space(4.0);
    kit::hairline(ui);
    ui.add_space(6.0);
    kit::subtitle(ui, "Add to your GitHub account");
    let scope = match &f.target {
        UploadTarget::Ssh(_) => {
            if form::radio(
                ui,
                f.key_use == KeyUse::Signing,
                "As a signing key: Verified on commits signed with it",
            ) {
                f.key_use = KeyUse::Signing;
            }
            if form::radio(
                ui,
                f.key_use == KeyUse::Authentication,
                "As an authentication key: git push and pull over SSH",
            ) {
                f.key_use = KeyUse::Authentication;
            }
            f.key_use.scope()
        }
        UploadTarget::Gpg(_) => "admin:gpg_key",
    };
    form::field(
        ui,
        "Title",
        Some("How the key is listed on github.com/settings/keys."),
        |ui| {
            form::input(
                ui,
                "identity_upload_title",
                &mut f.title,
                InputOpts::new("My laptop").width(360.0),
            );
        },
    );
    // Does the token have the scope? Only known once the account was read.
    let active = up.account.and_then(|a| a.report.active());
    match active.map(|a| a.scopes.clone()) {
        Some(Some(have)) => {
            let ok = match scope {
                "admin:gpg_key" => account::grants(&have, "admin:gpg_key")
                    .or(account::grants(&have, "write:gpg_key")),
                "admin:ssh_signing_key" => account::grants(&have, scope)
                    .or(account::grants(&have, "write:ssh_signing_key")),
                _ => account::grants(&have, scope).or(account::grants(&have, "write:public_key")),
            };
            if ok.is_none() {
                kit::callout(
                    ui,
                    Tone::Warning,
                    &format!("gh's token lacks {scope}"),
                    "GitHub will refuse the upload. Add the scope first (it opens your browser):",
                );
                kit::code_block(
                    ui,
                    &account::refresh_command("github.com", &[scope]),
                    actions,
                );
            }
        }
        Some(None) => {
            kit::secondary(
                ui,
                "gh does not report this token's scopes; GitHub checks them when the key is sent.",
            );
        }
        None => {
            kit::secondary(
                ui,
                &format!(
                    "gh needs the {scope} scope; Settings → GitHub account shows what the token has."
                ),
            );
        }
    }
    if up.safe {
        kit::callout(
            ui,
            Tone::Warning,
            "Safe mode is on",
            "Adding a key changes your GitHub account, so gitvis refuses it. Turn safe mode off in Settings → Safety first.",
        );
    }
    let line = match &f.target {
        UploadTarget::Ssh(file) => gitvis_core::devops::identity::cmd_line(
            "gh",
            &ssh::upload_args(file, f.key_use, &f.title),
        ),
        UploadTarget::Gpg(id) => format!(
            "gpg --armor --export {id} | gh gpg-key add - --title \"{}\"",
            f.title.trim()
        ),
    };
    kit::code_block(ui, &line, actions);
    form::actions_row(ui, |ui| {
        if form::primary(
            ui,
            "Add to GitHub…",
            !up.safe && !up.busy && !f.title.trim().is_empty(),
        )
        .clicked()
        {
            actions.push(Act::SubmitUpload.into());
        }
        if form::button(ui, "Cancel", true).clicked() {
            actions.push(Act::CloseUpload.into());
        }
    });
}
