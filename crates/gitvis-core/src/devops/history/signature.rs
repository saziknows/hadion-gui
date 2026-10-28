//! Signature verification of commits.
//!
//! `git log --no-walk --stdin --format=%x1e%H%x1f%G?%x1f%GS%x1f%GK%x1f%GF
//! %x1f%GP%x1f%GT` verifies a batch (git runs gpg or ssh-keygen for each
//! signed commit; unsigned ones cost nothing). `%G?` alone is not enough:
//! an SSH-signed commit reads as `N` (no signature) when
//! `gpg.ssh.allowedSignersFile` is not set, so the signature kind is read
//! from the commit object itself (libgit2, in process) and such commits
//! are reported as "can't check" with the reason.

use std::collections::HashMap;
use std::path::Path;

use super::git_text;
use crate::Oid;

/// What `%G?` says, refined with the commit object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Status {
    /// `G`: good signature from a trusted key.
    Good,
    /// `U`: good signature, the key is not trusted (or, for SSH, not in the
    /// allowed signers for that identity).
    Untrusted,
    /// `X`: good signature that has expired.
    ExpiredSignature,
    /// `Y`: good signature made by a key that has expired.
    ExpiredKey,
    /// `R`: good signature made by a revoked key.
    Revoked,
    /// `B`: bad signature (the commit was changed after signing).
    Bad,
    /// `E`, or a signed commit git could not check (missing key, missing
    /// allowed signers file, missing gpg).
    CantCheck,
    /// `N` and no signature in the commit.
    Unsigned,
}

impl Status {
    pub fn from_code(c: &str) -> Status {
        match c.trim() {
            "G" => Status::Good,
            "U" => Status::Untrusted,
            "X" => Status::ExpiredSignature,
            "Y" => Status::ExpiredKey,
            "R" => Status::Revoked,
            "B" => Status::Bad,
            "E" => Status::CantCheck,
            _ => Status::Unsigned,
        }
    }

    /// The badge text in the graph (`None` for unsigned commits).
    pub fn badge(self) -> Option<&'static str> {
        Some(match self {
            Status::Good => "Verified",
            Status::Untrusted => "Untrusted",
            Status::ExpiredSignature | Status::ExpiredKey => "Expired",
            Status::Revoked => "Revoked",
            Status::Bad => "Bad signature",
            Status::CantCheck => "Unverified",
            Status::Unsigned => return None,
        })
    }

    /// A sentence for the commit panel.
    pub fn label(self) -> &'static str {
        match self {
            Status::Good => "Good signature from a trusted key",
            Status::Untrusted => "Good signature, but the key is not trusted",
            Status::ExpiredSignature => "Good signature that has expired",
            Status::ExpiredKey => "Good signature made by a key that has since expired",
            Status::Revoked => "Signature made by a revoked key",
            Status::Bad => "Bad signature: the commit does not match what was signed",
            Status::CantCheck => "Signed, but the signature could not be checked",
            Status::Unsigned => "Not signed",
        }
    }

    pub fn level(self) -> Level {
        match self {
            Status::Good => Level::Good,
            Status::Untrusted | Status::ExpiredSignature | Status::ExpiredKey => Level::Warn,
            Status::Revoked | Status::Bad => Level::Bad,
            Status::CantCheck => Level::Unknown,
            Status::Unsigned => Level::None,
        }
    }
}

/// How the app colours a status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Good,
    Warn,
    Bad,
    Unknown,
    None,
}

/// The kind of signature in the commit object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Gpg,
    Ssh,
    X509,
    Other,
}

impl Kind {
    pub fn from_armor(sig: &[u8]) -> Kind {
        let head = String::from_utf8_lossy(&sig[..sig.len().min(64)]);
        if head.contains("PGP") {
            Kind::Gpg
        } else if head.contains("SSH") {
            Kind::Ssh
        } else if head.contains("SIGNED MESSAGE") || head.contains("PKCS7") {
            Kind::X509
        } else {
            Kind::Other
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Gpg => "GPG",
            Kind::Ssh => "SSH",
            Kind::X509 => "X.509",
            Kind::Other => "Unknown kind",
        }
    }
}

/// The verification of one commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sig {
    pub status: Status,
    pub kind: Option<Kind>,
    /// `%GS`: the signer (name and e-mail for GPG, the principal for SSH).
    pub signer: String,
    /// `%GK`: the key id (GPG) or key fingerprint (SSH).
    pub key: String,
    /// `%GF`: the fingerprint of the signing key.
    pub fingerprint: String,
    /// `%GP`: the primary key's fingerprint when a subkey signed.
    pub primary: String,
    /// `%GT`: `ultimate`, `fully`, `marginal`, `never`, `undefined`.
    pub trust: String,
    /// `%GG`: what gpg / ssh-keygen said (detail queries only).
    pub message: String,
}

impl Sig {
    pub fn unsigned() -> Sig {
        Sig {
            status: Status::Unsigned,
            kind: None,
            signer: String::new(),
            key: String::new(),
            fingerprint: String::new(),
            primary: String::new(),
            trust: String::new(),
            message: String::new(),
        }
    }
}

pub const BATCH_FORMAT: &str = "--format=%x1e%H%x1f%G?%x1f%GS%x1f%GK%x1f%GF%x1f%GP%x1f%GT";
pub const DETAIL_FORMAT: &str = "--format=%x1e%H%x1f%G?%x1f%GS%x1f%GK%x1f%GF%x1f%GP%x1f%GT%x1f%GG";

pub fn args(detail: bool) -> Vec<String> {
    vec![
        "log".into(),
        "--no-walk=unsorted".into(),
        "--stdin".into(),
        if detail { DETAIL_FORMAT } else { BATCH_FORMAT }.into(),
    ]
}

/// Parse `%x1e`-separated records.
pub fn parse(text: &str) -> Vec<(String, Sig)> {
    let mut out = Vec::new();
    for rec in text.split('\u{1e}') {
        let rec = rec.trim_start_matches(['\n', '\r']);
        if rec.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = rec.splitn(8, '\u{1f}').collect();
        if f.len() < 7 {
            continue;
        }
        let trust = f[6].trim();
        out.push((
            f[0].trim().to_owned(),
            Sig {
                status: Status::from_code(f[1]),
                kind: None,
                signer: f[2].trim().to_owned(),
                key: f[3].trim().to_owned(),
                fingerprint: f[4].trim().to_owned(),
                primary: f[5].trim().to_owned(),
                // git prints `undefined` for unsigned commits too.
                trust: trust.to_owned(),
                message: f.get(7).map(|m| m.trim().to_owned()).unwrap_or_default(),
            },
        ));
    }
    out
}

/// Combine `%G?` with the signature found in the commit object.
pub fn refine(mut sig: Sig, kind: Option<Kind>) -> Sig {
    sig.kind = kind;
    if sig.status == Status::Unsigned && kind.is_some() {
        sig.status = Status::CantCheck;
    }
    if sig.status == Status::Unsigned {
        sig.trust.clear();
    }
    sig
}

/// The signature kind of each commit, read with libgit2.
pub fn kinds(dir: &Path, shas: &[String]) -> HashMap<String, Kind> {
    let mut out = HashMap::new();
    let Ok(repo) = git2::Repository::discover(dir) else {
        return out;
    };
    for s in shas {
        if let Ok(oid) = Oid::from_str(s)
            && let Ok((sig, _)) = repo.extract_signature(&oid, None)
        {
            out.insert(s.clone(), Kind::from_armor(&sig));
        }
    }
    out
}

/// What verification depends on in the git configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Setup {
    /// `gpg.format` (`openpgp` when unset).
    pub format: String,
    /// `gpg.ssh.allowedSignersFile`.
    pub allowed_signers: Option<String>,
    /// The file exists.
    pub allowed_signers_exists: bool,
    /// `gpg.program` (or `gpg.ssh.program`).
    pub gpg_program: Option<String>,
    /// What git printed on stderr while verifying (e.g. "gpg.ssh.
    /// allowedSignersFile needs to be configured").
    pub stderr: String,
}

/// Read the setup from `git config`.
pub fn setup(dir: &Path, cmds: &mut Vec<String>) -> Setup {
    let get = |key: &str, cmds: &mut Vec<String>| {
        git_text(dir, &["config", "--get", key], cmds)
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    };
    let allowed = get("gpg.ssh.allowedSignersFile", cmds);
    let exists = allowed.as_deref().is_some_and(|p| {
        let expanded = expand_home(p);
        let path = Path::new(&expanded);
        if path.is_absolute() {
            path.is_file()
        } else {
            dir.join(path).is_file()
        }
    });
    Setup {
        format: get("gpg.format", cmds).unwrap_or_else(|| "openpgp".into()),
        allowed_signers: allowed,
        allowed_signers_exists: exists,
        gpg_program: get("gpg.program", cmds),
        stderr: String::new(),
    }
}

fn expand_home(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_default();
        return format!("{home}/{rest}");
    }
    p.to_owned()
}

/// A verified batch, and what git printed on stderr meanwhile.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Verified {
    pub sigs: Vec<(String, Sig)>,
    pub stderr: String,
}

/// Verify a batch of commits (SHAs) and refine with the commit objects.
pub fn verify(
    dir: &Path,
    shas: &[String],
    detail: bool,
    cmds: &mut Vec<String>,
) -> Result<Verified, String> {
    if shas.is_empty() {
        return Ok(Verified::default());
    }
    let mut input = shas.join("\n");
    input.push('\n');
    let a = args(detail);
    cmds.push(format!(
        "{}  (standard input: {} commits)",
        super::git_line(&a),
        shas.len()
    ));
    let out = crate::cli::run(dir, &a, Some(input.as_bytes())).map_err(|e| e.to_string())?;
    let kinds = kinds(dir, shas);
    let mut got: HashMap<String, Sig> = parse(&String::from_utf8_lossy(&out.stdout))
        .into_iter()
        .collect();
    Ok(Verified {
        sigs: shas
            .iter()
            .map(|s| {
                let sig = got.remove(s).unwrap_or_else(Sig::unsigned);
                (s.clone(), refine(sig, kinds.get(s).copied()))
            })
            .collect(),
        stderr: out.stderr,
    })
}

/// GitHub signs commits made on github.com (web merges, edits) with these
/// keys ("web-flow").
pub const GITHUB_KEYS: [&str; 2] = ["B5690EEEBB952194", "4AEE18F83AFDEB23"];

/// Why a signature is not "verified", and how to fix that.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Advice {
    pub title: String,
    pub body: String,
    /// Commands to run in a terminal (never run by gitvis).
    pub fix: Vec<String>,
}

/// Advice for a status that is not plainly good. `email` is the author's
/// e-mail (the identity an SSH allowed-signers line needs).
pub fn advise(sig: &Sig, setup: &Setup, email: &str) -> Option<Advice> {
    let email = if email.trim().is_empty() {
        "someone@example.com"
    } else {
        email.trim()
    };
    let ssh = sig.kind == Some(Kind::Ssh) || sig.key.starts_with("SHA256:");
    let github = GITHUB_KEYS
        .iter()
        .any(|k| sig.key.eq_ignore_ascii_case(k) || sig.fingerprint.ends_with(k));
    match sig.status {
        Status::Good | Status::Unsigned => None,
        Status::CantCheck | Status::Untrusted if ssh && !setup.allowed_signers_exists => Some(Advice {
            title: "SSH signatures need an allowed-signers file".into(),
            body: match &setup.allowed_signers {
                Some(p) => format!(
                    "gpg.ssh.allowedSignersFile points at {p}, which does not exist. Git checks SSH signatures against that file: one line per trusted key, the signer's e-mail and then the public key."
                ),
                None => "Git checks SSH signatures against a list of trusted keys, and gpg.ssh.allowedSignersFile is not set, so this commit cannot be checked. Create the file (one line per trusted key: the signer's e-mail, then the public key) and point git at it.".into(),
            },
            fix: vec![
                format!(
                    "echo \"{email} $(cat ~/.ssh/id_ed25519.pub)\" >> ~/.ssh/allowed_signers"
                ),
                "git config --global gpg.ssh.allowedSignersFile ~/.ssh/allowed_signers".into(),
            ],
        }),
        Status::CantCheck if github => Some(Advice {
            title: "GitHub's signing key is not in your keyring".into(),
            body: format!(
                "GitHub signed this commit (made on github.com: a web merge, an edit or a suggestion) with its web-flow key {}. gpg has no copy of that public key, so it cannot check the signature. Import it once:",
                sig.key
            ),
            fix: vec!["curl -s https://github.com/web-flow.gpg | gpg --import".into()],
        }),
        Status::CantCheck if ssh => Some(Advice {
            title: "The signature could not be checked".into(),
            body: if setup.stderr.trim().is_empty() {
                "ssh-keygen could not verify the signature. Check that ssh-keygen is on PATH and the allowed-signers file is readable.".into()
            } else {
                setup.stderr.trim().to_owned()
            },
            fix: Vec::new(),
        }),
        Status::CantCheck => Some(Advice {
            title: "The signer's public key is missing".into(),
            body: if sig.key.is_empty() {
                "gpg could not check the signature (is gpg installed and on PATH?). git uses gpg.program, or gpg from PATH.".into()
            } else {
                format!(
                    "gpg does not have the public key {} that made this signature. Import it from a key server, or from the signer (GitHub serves every user's keys at https://github.com/USER.gpg):",
                    sig.key
                )
            },
            fix: if sig.key.is_empty() {
                Vec::new()
            } else {
                vec![format!("gpg --recv-keys {}", sig.key)]
            },
        }),
        Status::Untrusted if ssh => Some(Advice {
            title: "The key is not in your allowed signers".into(),
            body: format!(
                "The signature is good, but no line of the allowed-signers file{} matches this key for {email}. Add one if you trust it:",
                setup
                    .allowed_signers
                    .as_deref()
                    .map(|p| format!(" ({p})"))
                    .unwrap_or_default()
            ),
            fix: vec![format!(
                "echo \"{email} <the signer's public key>\" >> {}",
                setup
                    .allowed_signers
                    .as_deref()
                    .unwrap_or("~/.ssh/allowed_signers")
            )],
        }),
        Status::Untrusted => Some(Advice {
            title: "The key is not trusted".into(),
            body: format!(
                "The signature matches key {}, but you have not certified that key, so gpg does not know whether it belongs to {}. Sign it locally once you have checked its fingerprint with its owner:",
                sig.key,
                if sig.signer.is_empty() { "its owner" } else { &sig.signer }
            ),
            fix: vec![format!("gpg --lsign-key {}", if sig.fingerprint.is_empty() { &sig.key } else { &sig.fingerprint })],
        }),
        Status::ExpiredSignature => Some(Advice {
            title: "The signature has expired".into(),
            body: "The signature was valid but was made with an expiry date that has passed. The commit itself is unchanged.".into(),
            fix: Vec::new(),
        }),
        Status::ExpiredKey => Some(Advice {
            title: "The key has expired".into(),
            body: format!(
                "Key {} expired after it signed this commit. If its owner extended it, refresh your copy:",
                sig.key
            ),
            fix: vec![format!("gpg --refresh-keys {}", sig.key)],
        }),
        Status::Revoked => Some(Advice {
            title: "The key was revoked".into(),
            body: format!(
                "Key {} was revoked by its owner, usually because it was lost or compromised. Treat this signature as untrusted.",
                sig.key
            ),
            fix: Vec::new(),
        }),
        Status::Bad => Some(Advice {
            title: "The signature does not match".into(),
            body: "The commit's content differs from what was signed: it was changed after signing, or the signature was copied from another commit. Do not trust it.".into(),
            fix: Vec::new(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_batch_and_detail() {
        let text = "\u{1e}184e35392920b397f50720fcbb20358feb61e0bf\u{1f}N\u{1f}\u{1f}\u{1f}\u{1f}\u{1f}undefined\n\u{1e}5a6b15f5467b8e2501f0341576ceb1316ff13596\u{1f}E\u{1f}\u{1f}B5690EEEBB952194\u{1f}\u{1f}\u{1f}undefined\n\u{1e}eea71ca16b12acf6414d30547de585fe0e69773c\u{1f}G\u{1f}test@example.com\u{1f}SHA256:Ftk\u{1f}SHA256:Ftk\u{1f}\u{1f}fully\u{1f}Good \"git\" signature for test@example.com\nwith more\n";
        let v = parse(text);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].1.status, Status::Unsigned);
        assert_eq!(v[1].1.status, Status::CantCheck);
        assert_eq!(v[1].1.key, "B5690EEEBB952194");
        assert_eq!(v[2].1.status, Status::Good);
        assert_eq!(v[2].1.signer, "test@example.com");
        assert_eq!(v[2].1.trust, "fully");
        assert!(v[2].1.message.contains("with more"));
    }

    #[test]
    fn badges_and_levels() {
        let codes = ["G", "U", "X", "Y", "R", "B", "E", "N", ""];
        let badges: Vec<Option<&str>> =
            codes.iter().map(|c| Status::from_code(c).badge()).collect();
        assert_eq!(
            badges,
            vec![
                Some("Verified"),
                Some("Untrusted"),
                Some("Expired"),
                Some("Expired"),
                Some("Revoked"),
                Some("Bad signature"),
                Some("Unverified"),
                None,
                None
            ]
        );
        assert_eq!(Status::Good.level(), Level::Good);
        assert_eq!(Status::Untrusted.level(), Level::Warn);
        assert_eq!(Status::Bad.level(), Level::Bad);
        assert_eq!(Status::CantCheck.level(), Level::Unknown);
        assert_eq!(Status::Unsigned.level(), Level::None);
    }

    #[test]
    fn ssh_without_allowed_signers_is_cant_check() {
        let mut s = Sig::unsigned();
        s.trust = "undefined".into();
        let r = refine(s.clone(), Some(Kind::Ssh));
        assert_eq!(r.status, Status::CantCheck);
        let r = refine(s, None);
        assert_eq!(r.status, Status::Unsigned);
        assert!(r.trust.is_empty());
        let advice = advise(
            &refine(Sig::unsigned(), Some(Kind::Ssh)),
            &Setup::default(),
            "me@x",
        )
        .unwrap();
        assert!(advice.title.contains("allowed-signers"));
        assert!(
            advice
                .fix
                .iter()
                .any(|f| f.contains("gpg.ssh.allowedSignersFile"))
        );
        assert!(advice.fix[0].contains("me@x"));
    }

    #[test]
    fn github_and_missing_keys() {
        let mut s = Sig::unsigned();
        s.status = Status::CantCheck;
        s.key = "B5690EEEBB952194".into();
        s.kind = Some(Kind::Gpg);
        let a = advise(&s, &Setup::default(), "").unwrap();
        assert!(a.fix[0].contains("web-flow.gpg"));
        s.key = "0123456789ABCDEF".into();
        let a = advise(&s, &Setup::default(), "").unwrap();
        assert_eq!(a.fix, vec!["gpg --recv-keys 0123456789ABCDEF"]);
        s.status = Status::Good;
        assert!(advise(&s, &Setup::default(), "").is_none());
        s.status = Status::Untrusted;
        s.fingerprint = "FPR".into();
        assert_eq!(
            advise(&s, &Setup::default(), "").unwrap().fix,
            vec!["gpg --lsign-key FPR"]
        );
    }

    #[test]
    fn kind_from_armor() {
        assert_eq!(
            Kind::from_armor(b"-----BEGIN PGP SIGNATURE-----\n"),
            Kind::Gpg
        );
        assert_eq!(
            Kind::from_armor(b"-----BEGIN SSH SIGNATURE-----\n"),
            Kind::Ssh
        );
        assert_eq!(
            Kind::from_armor(b"-----BEGIN SIGNED MESSAGE-----"),
            Kind::X509
        );
        assert_eq!(args(true).last().unwrap(), DETAIL_FORMAT);
    }
}
