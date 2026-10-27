//! Commit signing setup as a list of commands: the signature format, the
//! key, `commit.gpgsign` (and `tag.gpgSign`), and for SSH an
//! allowed-signers file with your own e-mail and key so that `git log
//! --show-signature` verifies your signatures locally.
//!
//! A signature proves who made a commit; GitHub shows "Verified" when the
//! key is on the committer's account (as a signing key for SSH).

use std::path::{Path, PathBuf};

use super::gpg;
use super::ssh::{self, PubKey};
use super::{Scope, Step, slash, tilde};

/// The kind of key that signs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Format {
    #[default]
    Ssh,
    Gpg,
}

impl Format {
    /// The value of `gpg.format`.
    pub fn value(self) -> &'static str {
        match self {
            Format::Ssh => "ssh",
            Format::Gpg => "openpgp",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Ssh => "SSH key",
            Format::Gpg => "GPG key",
        }
    }
}

/// The allowed-signers part of an SSH setup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Allowed {
    pub file: PathBuf,
    /// What `gpg.ssh.allowedSignersFile` gets (`~/.ssh/allowed_signers`).
    pub config_value: String,
    pub line: String,
}

/// A complete signing setup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setup {
    pub format: Format,
    pub scope: Scope,
    /// What `user.signingkey` gets: a key file path or a GPG key id.
    pub key: String,
    pub sign_tags: bool,
    pub allowed: Option<Allowed>,
    /// `gpg.program`, when the gpg that listed the key is not the one git
    /// would pick by itself.
    pub gpg_program: Option<String>,
}

/// An SSH setup for `key`. `use_private` points `user.signingkey` at the
/// private key file (works without an agent) instead of the public one
/// (needs the key loaded in ssh-agent).
pub fn ssh_setup(
    key: &PubKey,
    use_private: bool,
    email: &str,
    scope: Scope,
    sign_tags: bool,
    ssh_dir: &Path,
    home: Option<&Path>,
) -> Setup {
    let file = ssh_dir.join("allowed_signers");
    Setup {
        format: Format::Ssh,
        scope,
        key: slash(if use_private && key.has_private {
            &key.private
        } else {
            &key.file
        }),
        sign_tags,
        allowed: Some(Allowed {
            config_value: tilde(&file, home),
            file,
            line: ssh::allowed_signers_line(email, &key.algo, &key.blob, &key.comment),
        }),
        gpg_program: None,
    }
}

/// A GPG setup for `key`, found by `found`.
pub fn gpg_setup(
    key: &gpg::Key,
    found: Option<&gpg::Found>,
    scope: Scope,
    sign_tags: bool,
) -> Setup {
    Setup {
        format: Format::Gpg,
        scope,
        key: key.keyid.clone(),
        sign_tags,
        allowed: None,
        gpg_program: found
            .filter(|f| f.how != "Git for Windows")
            .map(|f| slash(&f.program)),
    }
}

fn set(flag: &str, key: &str, value: &str) -> Step {
    Step::Git(vec!["config".into(), flag.into(), key.into(), value.into()])
}

/// The commands of a setup, in order.
pub fn plan(s: &Setup) -> Result<Vec<Step>, String> {
    if !s.scope.editable() {
        return Err("Signing can be set up globally or for this repository".to_owned());
    }
    if s.key.trim().is_empty() {
        return Err("Choose a key first".to_owned());
    }
    let flag = s.scope.flag().unwrap_or("--global");
    let mut steps = vec![
        set(flag, "gpg.format", s.format.value()),
        set(flag, "user.signingkey", s.key.trim()),
        set(flag, "commit.gpgsign", "true"),
    ];
    if s.sign_tags {
        steps.push(set(flag, "tag.gpgSign", "true"));
    }
    if let Some(p) = &s.gpg_program {
        steps.push(set(flag, "gpg.program", p));
    }
    if let Some(a) = &s.allowed {
        let email = a.line.split_whitespace().next().unwrap_or("");
        if email.is_empty() || !email.contains('@') || email.contains(',') {
            return Err("Enter your e-mail for the allowed-signers file".to_owned());
        }
        if let Some(dir) = a.file.parent()
            && !dir.as_os_str().is_empty()
            && !dir.exists()
        {
            steps.push(Step::MakeDir(dir.to_path_buf()));
        }
        steps.push(Step::AppendLine {
            path: a.file.clone(),
            line: a.line.clone(),
        });
        steps.push(set(flag, "gpg.ssh.allowedSignersFile", &a.config_value));
    }
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devops::identity::ssh::Fingerprint;

    fn key() -> PubKey {
        PubKey {
            file: PathBuf::from("C:/Users/me/.ssh/id_ed25519.pub"),
            private: PathBuf::from("C:/Users/me/.ssh/id_ed25519"),
            has_private: true,
            algo: "ssh-ed25519".into(),
            blob: "AAAAC3".into(),
            comment: "laptop".into(),
            fingerprint: Fingerprint {
                bits: Some(256),
                hash: "SHA256:x".into(),
                comment: "laptop".into(),
                kind: "ED25519".into(),
            },
            error: None,
        }
    }

    #[test]
    fn ssh_plan() {
        let home = Path::new("C:/Users/me");
        let dir = Path::new("C:/Users/me/.ssh-test-missing");
        let s = ssh_setup(
            &key(),
            true,
            "me@x.example",
            Scope::Global,
            true,
            dir,
            Some(home),
        );
        assert_eq!(s.key, "C:/Users/me/.ssh/id_ed25519");
        let lines: Vec<String> = plan(&s).unwrap().iter().map(Step::display).collect();
        assert_eq!(
            lines,
            vec![
                "git config --global gpg.format ssh".to_owned(),
                "git config --global user.signingkey C:/Users/me/.ssh/id_ed25519".to_owned(),
                "git config --global commit.gpgsign true".to_owned(),
                "git config --global tag.gpgSign true".to_owned(),
                "create folder C:/Users/me/.ssh-test-missing".to_owned(),
                "append to C:/Users/me/.ssh-test-missing/allowed_signers:\n    me@x.example namespaces=\"git\" ssh-ed25519 AAAAC3 laptop".to_owned(),
                "git config --global gpg.ssh.allowedSignersFile ~/.ssh-test-missing/allowed_signers".to_owned(),
            ]
        );
        let public = ssh_setup(
            &key(),
            false,
            "me@x.example",
            Scope::Local,
            false,
            dir,
            None,
        );
        assert_eq!(public.key, "C:/Users/me/.ssh/id_ed25519.pub");
        let p = plan(&public).unwrap();
        assert!(p[0].display().starts_with("git config --local"));
        assert_eq!(p.len(), 6, "no tag signing");
        let mut bad = s.clone();
        bad.allowed.as_mut().unwrap().line = "  namespaces=\"git\" ssh-ed25519 AAAA".into();
        assert!(plan(&bad).is_err());
        let mut sys = s;
        sys.scope = Scope::System;
        assert!(plan(&sys).is_err());
    }

    #[test]
    fn gpg_plan() {
        let k = gpg::parse_colons(
            "sec:u:3072:1:B91FBCB5172A8901:1793137445:::u:::scSC:::+:::23::0:\nuid:u::::1::x::W <w@x.example>::::\n",
        )
        .remove(0);
        let found = gpg::Found {
            program: PathBuf::from("C:/Program Files (x86)/GnuPG/bin/gpg.exe"),
            how: "PATH",
        };
        let s = gpg_setup(&k, Some(&found), Scope::Global, false);
        let lines: Vec<String> = plan(&s).unwrap().iter().map(Step::display).collect();
        assert_eq!(
            lines,
            vec![
                "git config --global gpg.format openpgp",
                "git config --global user.signingkey B91FBCB5172A8901",
                "git config --global commit.gpgsign true",
                "git config --global gpg.program \"C:/Program Files (x86)/GnuPG/bin/gpg.exe\"",
            ]
        );
        let bundled = gpg::Found {
            program: PathBuf::from("C:/Program Files/Git/usr/bin/gpg.exe"),
            how: "Git for Windows",
        };
        assert_eq!(
            gpg_setup(&k, Some(&bundled), Scope::Global, true).gpg_program,
            None
        );
    }
}
