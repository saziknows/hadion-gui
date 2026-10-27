//! SSH keys: the public keys in `~/.ssh` (`ssh-keygen -lf`), the agent
//! (`ssh-add -l`), the GitHub access test (`ssh -T git@github.com`), key
//! generation and the allowed-signers line that makes SSH signatures
//! verify locally.
//!
//! Passphrases never go on a command line: a key is either generated
//! without one (the form says what that means), or the user gets the exact
//! command to run in a terminal, where ssh-keygen asks for it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::{Step, cmd_line, slash, tool};

/// One line of `ssh-keygen -lf` or `ssh-add -l`:
/// `256 SHA256:abc… comment words (ED25519)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fingerprint {
    pub bits: Option<u32>,
    /// `SHA256:…`.
    pub hash: String,
    pub comment: String,
    /// `ED25519`, `RSA`, `ECDSA`, `ED25519-SK`.
    pub kind: String,
}

/// Parse one fingerprint line.
pub fn parse_fingerprint(line: &str) -> Option<Fingerprint> {
    let line = line.trim();
    let (bits, rest) = line.split_once(' ')?;
    let (hash, rest) = match rest.split_once(' ') {
        Some((h, r)) => (h, r.trim()),
        None => (rest, ""),
    };
    if !hash.contains(':') {
        return None;
    }
    let (comment, kind) = match (rest.rfind('('), rest.ends_with(')')) {
        (Some(i), true) => (rest[..i].trim(), &rest[i + 1..rest.len() - 1]),
        _ => (rest, ""),
    };
    Some(Fingerprint {
        bits: bits.parse().ok(),
        hash: hash.to_owned(),
        comment: if comment == "no comment" {
            String::new()
        } else {
            comment.to_owned()
        },
        kind: kind.to_owned(),
    })
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 decoding (padding optional).
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut n = 0;
    for c in s.trim_end_matches('=').bytes() {
        let v = B64.iter().position(|b| *b == c)? as u32;
        acc = (acc << 6) | v;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((acc >> n) as u8);
            acc &= (1 << n) - 1;
        }
    }
    Some(out)
}

/// Standard base64 without padding (OpenSSH's fingerprint encoding).
pub fn base64_encode_nopad(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let v = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        let chars = chunk.len() + 1;
        for i in 0..chars {
            s.push(B64[((v >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    s
}

/// `SHA256:…` of a public key's base64 blob, as OpenSSH prints it.
pub fn fingerprint_of(blob: &str) -> Option<String> {
    let bytes = base64_decode(blob)?;
    Some(format!(
        "SHA256:{}",
        base64_encode_nopad(&Sha256::digest(&bytes))
    ))
}

/// Key size from the blob, for when ssh-keygen is missing.
pub fn blob_bits(algo: &str, blob: &str) -> Option<u32> {
    match algo {
        a if a.contains("ed25519") => Some(256),
        "ecdsa-sha2-nistp256" | "sk-ecdsa-sha2-nistp256@openssh.com" => Some(256),
        "ecdsa-sha2-nistp384" => Some(384),
        "ecdsa-sha2-nistp521" => Some(521),
        "ssh-rsa" => {
            let b = base64_decode(blob)?;
            // string "ssh-rsa", mpint e, mpint n
            let mut pos = 0usize;
            let read = |pos: &mut usize| -> Option<Vec<u8>> {
                let len = u32::from_be_bytes(b.get(*pos..*pos + 4)?.try_into().ok()?) as usize;
                let v = b.get(*pos + 4..*pos + 4 + len)?.to_vec();
                *pos += 4 + len;
                Some(v)
            };
            read(&mut pos)?;
            read(&mut pos)?;
            let n = read(&mut pos)?;
            let first = n.iter().position(|x| *x != 0)?;
            let lead = n[first].leading_zeros();
            Some(((n.len() - first) as u32) * 8 - lead)
        }
        _ => None,
    }
}

/// `(algorithm, base64 blob, comment)` of a `.pub` file.
pub fn parse_pub(text: &str) -> Option<(String, String, String)> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))?;
    let mut parts = line.splitn(3, char::is_whitespace);
    let algo = parts.next()?.to_owned();
    let blob = parts.next()?.trim().to_owned();
    if !(algo.starts_with("ssh-") || algo.starts_with("ecdsa-") || algo.starts_with("sk-")) {
        return None;
    }
    base64_decode(&blob)?;
    Some((algo, blob, parts.next().unwrap_or("").trim().to_owned()))
}

/// A public key file in `~/.ssh`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PubKey {
    pub file: PathBuf,
    /// The same path without `.pub`.
    pub private: PathBuf,
    pub has_private: bool,
    /// `ssh-ed25519`.
    pub algo: String,
    pub blob: String,
    pub comment: String,
    /// From `ssh-keygen -lf`, or computed by gitvis when it is missing.
    pub fingerprint: Fingerprint,
    /// `ssh-keygen` printed this instead of a fingerprint.
    pub error: Option<String>,
}

impl PubKey {
    /// `ED25519`, `RSA`, `ECDSA`.
    pub fn kind(&self) -> String {
        if !self.fingerprint.kind.is_empty() {
            return self.fingerprint.kind.clone();
        }
        algo_kind(&self.algo)
    }

    /// The public key line (`ssh-ed25519 AAAA… comment`).
    pub fn line(&self) -> String {
        if self.comment.is_empty() {
            format!("{} {}", self.algo, self.blob)
        } else {
            format!("{} {} {}", self.algo, self.blob, self.comment)
        }
    }

    /// The file name (`id_ed25519.pub`).
    pub fn name(&self) -> String {
        self.file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Is this the key `user.signingkey` names (a path to either half, or
    /// the key itself as `key::…`)?
    pub fn is(&self, signing_key: &str, home: Option<&Path>) -> bool {
        let k = signing_key.trim();
        if let Some(lit) = k.strip_prefix("key::") {
            return lit.split_whitespace().nth(1) == Some(self.blob.as_str());
        }
        if k.starts_with("ssh-") || k.starts_with("ecdsa-") {
            return k.split_whitespace().nth(1) == Some(self.blob.as_str());
        }
        let p = super::expand_tilde(k, home);
        super::same_path(&p, &self.file) || super::same_path(&p, &self.private)
    }
}

/// `ED25519` for `ssh-ed25519`.
pub fn algo_kind(algo: &str) -> String {
    match algo {
        "ssh-ed25519" => "ED25519".to_owned(),
        "ssh-rsa" => "RSA".to_owned(),
        "ssh-dss" => "DSA".to_owned(),
        a if a.starts_with("ecdsa-") => "ECDSA".to_owned(),
        a if a.starts_with("sk-ssh-ed25519") => "ED25519-SK".to_owned(),
        a if a.starts_with("sk-ecdsa") => "ECDSA-SK".to_owned(),
        a => a.to_ascii_uppercase(),
    }
}

/// Every `*.pub` in `dir`, sorted by name, with `ssh-keygen -lf` run on
/// each (`keygen` is its resolved path; without it the fingerprint is
/// computed here).
pub fn list(dir: &Path, keygen: Option<&str>, cmds: &mut Vec<String>) -> Vec<PubKey> {
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .is_some_and(|x| x.to_string_lossy().eq_ignore_ascii_case("pub"))
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    files.sort();
    let mut out = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let Some((algo, blob, comment)) = parse_pub(&text) else {
            continue;
        };
        let private = file.with_extension("");
        let computed = Fingerprint {
            bits: blob_bits(&algo, &blob),
            hash: fingerprint_of(&blob).unwrap_or_default(),
            comment: comment.clone(),
            kind: algo_kind(&algo),
        };
        let (fingerprint, error) = match keygen {
            Some(k) => {
                let args = vec!["-lf".to_owned(), slash(&file)];
                match tool(k, &args, dir, Duration::from_secs(10), cmds) {
                    Ok(c) if c.success() => match parse_fingerprint(&c.stdout_text()) {
                        Some(f) => (f, None),
                        None => (computed, Some(c.stdout_text().trim().to_owned())),
                    },
                    Ok(c) => (computed, Some(c.error_text())),
                    Err(e) => (computed, Some(e)),
                }
            }
            None => (computed, None),
        };
        out.push(PubKey {
            has_private: private.is_file(),
            private,
            file,
            algo,
            blob,
            comment,
            fingerprint,
            error,
        });
    }
    out
}

/// What `ssh-add -l` said about the agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Agent {
    Keys(Vec<Fingerprint>),
    /// Running, no keys loaded.
    Empty,
    /// No agent to talk to (the usual state on Windows).
    NotRunning(String),
    Error(String),
}

/// Interpret `ssh-add -l`: exit 0 lists keys, 1 means no identities, 2 (or
/// the Windows wording) means no agent.
pub fn parse_agent(code: Option<i32>, stdout: &str, stderr: &str) -> Agent {
    let text = format!("{}\n{}", stdout.trim(), stderr.trim())
        .trim()
        .to_owned();
    let lower = text.to_lowercase();
    if lower.contains("could not open a connection")
        || lower.contains("error connecting to agent")
        || lower.contains("communication with agent failed")
        || lower.contains("no such file or directory")
        || code == Some(2)
    {
        return Agent::NotRunning(text);
    }
    match code {
        Some(0) => {
            let keys: Vec<Fingerprint> = stdout.lines().filter_map(parse_fingerprint).collect();
            if keys.is_empty() {
                Agent::Empty
            } else {
                Agent::Keys(keys)
            }
        }
        Some(1) if lower.contains("no identities") => Agent::Empty,
        _ => Agent::Error(if text.is_empty() {
            format!("ssh-add exited with {code:?}")
        } else {
            text
        }),
    }
}

/// `ssh -T` to GitHub, never prompting; on the first connection the host
/// key is added to known_hosts (`accept-new`). `known_hosts` overrides the
/// file (tests).
pub fn test_args(known_hosts: Option<&str>) -> Vec<String> {
    let mut a: Vec<String> = [
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "StrictHostKeyChecking=accept-new",
        "-o",
        "ConnectTimeout=15",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    if let Some(k) = known_hosts {
        a.push("-o".into());
        a.push(format!("UserKnownHostsFile={k}"));
    }
    a.push("git@github.com".into());
    a
}

/// The outcome of the GitHub SSH test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TestResult {
    /// `Hi <user>! You've successfully authenticated…`
    Authenticated(String),
    /// No key GitHub knows (`Permission denied (publickey)`).
    Denied(String),
    /// The host key is unknown or changed.
    HostKey(String),
    /// DNS, timeout, refused, port 22 blocked.
    Network(String),
    Failed(String),
}

/// Read what `ssh -T git@github.com` printed (GitHub exits 1 even on
/// success, because it gives no shell).
pub fn parse_test(code: Option<i32>, stdout: &str, stderr: &str) -> TestResult {
    let text = format!("{}\n{}", stdout.trim(), stderr.trim())
        .trim()
        .to_owned();
    if let Some(i) = text.find("Hi ") {
        let rest = &text[i + 3..];
        if let Some(end) = rest.find('!') {
            let user = rest[..end].trim();
            if !user.is_empty() && !user.contains(char::is_whitespace) {
                return TestResult::Authenticated(user.to_owned());
            }
        }
    }
    let lower = text.to_lowercase();
    if lower.contains("permission denied") {
        TestResult::Denied(text)
    } else if lower.contains("host key verification failed")
        || lower.contains("remote host identification has changed")
        || lower.contains("no matching host key")
    {
        TestResult::HostKey(text)
    } else if lower.contains("could not resolve hostname")
        || lower.contains("timed out")
        || lower.contains("connection refused")
        || lower.contains("network is unreachable")
        || lower.contains("connection closed")
        || lower.contains("connection reset")
    {
        TestResult::Network(text)
    } else {
        TestResult::Failed(if text.is_empty() {
            format!("ssh exited with {code:?}")
        } else {
            text
        })
    }
}

/// Is `host` listed in a known_hosts file? `None` when it cannot be told
/// (the file has hashed entries).
pub fn known_host(path: &Path, host: &str) -> Option<bool> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Some(false);
    };
    let mut hashed = false;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let first = line.split_whitespace().next().unwrap_or("");
        let first = if first.starts_with('@') {
            line.split_whitespace().nth(1).unwrap_or("")
        } else {
            first
        };
        if first.starts_with("|1|") {
            hashed = true;
            continue;
        }
        if first
            .split(',')
            .any(|h| h.trim_start_matches('[').split(']').next() == Some(host))
        {
            return Some(true);
        }
    }
    if hashed { None } else { Some(false) }
}

/// `id_ed25519` in `dir`, or the first free `id_ed25519_N`.
pub fn next_key_path(dir: &Path) -> PathBuf {
    let base = dir.join("id_ed25519");
    if !base.exists() && !base.with_extension("pub").exists() {
        return base;
    }
    for n in 2..100 {
        let p = dir.join(format!("id_ed25519_{n}"));
        if !p.exists() && !dir.join(format!("id_ed25519_{n}.pub")).exists() {
            return p;
        }
    }
    dir.join("id_ed25519_new")
}

/// `ssh-keygen` arguments for a new ed25519 key. Without a passphrase the
/// empty one is given with `-N ""` (nothing secret on the command line);
/// with one, ssh-keygen must ask in a terminal.
pub fn generate_args(path: &Path, email: &str, with_passphrase: bool) -> Vec<String> {
    let mut a = vec![
        "-t".to_owned(),
        "ed25519".to_owned(),
        "-C".to_owned(),
        email.trim().to_owned(),
        "-f".to_owned(),
        slash(path),
    ];
    if !with_passphrase {
        a.push("-N".into());
        a.push(String::new());
    }
    a
}

/// The command to run in a terminal for a key with a passphrase.
pub fn terminal_command(path: &Path, email: &str) -> String {
    cmd_line("ssh-keygen", &generate_args(path, email, true))
}

/// The plan for a key without a passphrase: the folder (when missing) and
/// `ssh-keygen`. Refuses to overwrite an existing key.
pub fn plan_generate(keygen: &str, path: &Path, email: &str) -> Result<Vec<Step>, String> {
    if email.trim().is_empty() {
        return Err("Enter the e-mail for the key's comment".to_owned());
    }
    if path.as_os_str().is_empty() {
        return Err("Enter where the key goes".to_owned());
    }
    let pub_file = PathBuf::from(format!("{}.pub", path.to_string_lossy()));
    if path.exists() || pub_file.exists() {
        return Err(format!(
            "{} exists already: choose another file name, ssh-keygen would overwrite it",
            slash(path)
        ));
    }
    let mut steps = Vec::new();
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
        && !dir.exists()
    {
        steps.push(Step::MakeDir(dir.to_path_buf()));
    }
    steps.push(Step::Run {
        program: keygen.to_owned(),
        args: generate_args(path, email, false),
    });
    Ok(steps)
}

/// One line of an allowed-signers file: who may sign, for git only, with
/// which key (`ssh-keygen -Y verify` reads it).
pub fn allowed_signers_line(email: &str, algo: &str, blob: &str, comment: &str) -> String {
    let mut s = format!("{} namespaces=\"git\" {algo} {blob}", email.trim());
    if !comment.trim().is_empty() {
        s.push(' ');
        s.push_str(comment.trim());
    }
    s
}

/// What a key is uploaded to GitHub as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyUse {
    /// For `git push` / `git pull` over SSH.
    Authentication,
    /// For "Verified" commits.
    Signing,
}

impl KeyUse {
    pub fn arg(self) -> &'static str {
        match self {
            KeyUse::Authentication => "authentication",
            KeyUse::Signing => "signing",
        }
    }

    /// The token scope `gh ssh-key add` needs.
    pub fn scope(self) -> &'static str {
        match self {
            KeyUse::Authentication => "admin:public_key",
            KeyUse::Signing => "admin:ssh_signing_key",
        }
    }
}

/// `gh ssh-key add FILE --type signing --title TITLE`.
pub fn upload_args(file: &Path, key_use: KeyUse, title: &str) -> Vec<String> {
    vec![
        "ssh-key".into(),
        "add".into(),
        slash(file),
        "--type".into(),
        key_use.arg().into(),
        "--title".into(),
        title.trim().into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const ED: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMls1BMQMe96cYscbMw/Rxi/i5LB/09cVxNzrE0z22d/ test@example.com";

    #[test]
    fn fingerprint_lines() {
        let f = parse_fingerprint(
            "256 SHA256:soc6U0luvdUXLx79Ku8PFyMgSMwU5KLxoeIaBqjXy9c test@example.com (ED25519)\n",
        )
        .unwrap();
        assert_eq!(f.bits, Some(256));
        assert_eq!(f.hash, "SHA256:soc6U0luvdUXLx79Ku8PFyMgSMwU5KLxoeIaBqjXy9c");
        assert_eq!(f.comment, "test@example.com");
        assert_eq!(f.kind, "ED25519");
        let g = parse_fingerprint("3072 SHA256:Yid work key (RSA)").unwrap();
        assert_eq!(g.comment, "work key");
        let h = parse_fingerprint("256 SHA256:Zo no comment (ECDSA)").unwrap();
        assert_eq!(h.comment, "");
        assert_eq!(h.kind, "ECDSA");
        assert!(parse_fingerprint("The agent has no identities.").is_none());
    }

    #[test]
    fn computed_fingerprint_matches_ssh_keygen() {
        let (algo, blob, comment) = parse_pub(ED).unwrap();
        assert_eq!(algo, "ssh-ed25519");
        assert_eq!(comment, "test@example.com");
        // ssh-keygen -lf printed this for the same key.
        assert_eq!(
            fingerprint_of(&blob).unwrap(),
            "SHA256:soc6U0luvdUXLx79Ku8PFyMgSMwU5KLxoeIaBqjXy9c"
        );
        assert_eq!(blob_bits(&algo, &blob), Some(256));
        assert_eq!(base64_encode_nopad(b"ab"), "YWI");
        assert_eq!(base64_decode("YWI=").unwrap(), b"ab");
        assert!(parse_pub("not a key").is_none());
        assert!(parse_pub("ssh-ed25519 !!!").is_none());
    }

    #[test]
    fn rsa_bits_from_the_blob() {
        // A 2048-bit modulus: 0x00 prefix + 256 bytes with the top bit set.
        let mut b = Vec::new();
        let mut put = |v: &[u8]| {
            b.extend_from_slice(&(v.len() as u32).to_be_bytes());
            b.extend_from_slice(v);
        };
        put(b"ssh-rsa");
        put(&[1, 0, 1]);
        let mut n = vec![0u8];
        n.extend(std::iter::repeat_n(0xffu8, 256));
        put(&n);
        let blob = {
            let mut s = base64_encode_nopad(&b);
            while s.len() % 4 != 0 {
                s.push('=');
            }
            s
        };
        assert_eq!(blob_bits("ssh-rsa", &blob), Some(2048));
    }

    #[test]
    fn agent_states() {
        assert_eq!(
            parse_agent(
                Some(2),
                "",
                "Could not open a connection to your authentication agent."
            ),
            Agent::NotRunning("Could not open a connection to your authentication agent.".into())
        );
        assert!(matches!(
            parse_agent(
                Some(2),
                "",
                "Error connecting to agent: No such file or directory"
            ),
            Agent::NotRunning(_)
        ));
        assert_eq!(
            parse_agent(Some(1), "The agent has no identities.", ""),
            Agent::Empty
        );
        match parse_agent(
            Some(0),
            "256 SHA256:uwBK laptop (ED25519)\n3072 SHA256:Yid work (RSA)\n",
            "",
        ) {
            Agent::Keys(k) => {
                assert_eq!(k.len(), 2);
                assert_eq!(k[1].kind, "RSA");
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(parse_agent(Some(9), "", "boom"), Agent::Error(_)));
    }

    #[test]
    fn github_test_results() {
        assert_eq!(
            parse_test(
                Some(1),
                "",
                "Hi octocat! You've successfully authenticated, but GitHub does not provide shell access."
            ),
            TestResult::Authenticated("octocat".into())
        );
        assert!(matches!(
            parse_test(
                Some(255),
                "",
                "git@github.com: Permission denied (publickey)."
            ),
            TestResult::Denied(_)
        ));
        assert!(matches!(
            parse_test(Some(255), "", "Host key verification failed."),
            TestResult::HostKey(_)
        ));
        assert!(matches!(
            parse_test(
                Some(255),
                "",
                "ssh: connect to host github.com port 22: Connection timed out"
            ),
            TestResult::Network(_)
        ));
        assert!(matches!(
            parse_test(Some(255), "", ""),
            TestResult::Failed(_)
        ));
        let a = test_args(None);
        assert_eq!(a.first().map(String::as_str), Some("-T"));
        assert_eq!(a.last().map(String::as_str), Some("git@github.com"));
        assert!(a.contains(&"StrictHostKeyChecking=accept-new".to_owned()));
        assert!(test_args(Some("C:/t/kh")).contains(&"UserKnownHostsFile=C:/t/kh".to_owned()));
    }

    #[test]
    fn known_hosts_lookup() {
        let dir = std::env::temp_dir().join(format!("gitvis-id-kh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("known_hosts");
        std::fs::write(
            &f,
            "gitlab.com,1.2.3.4 ssh-ed25519 AAAA\n[github.com]:443 ssh-rsa AAAA\n",
        )
        .unwrap();
        assert_eq!(known_host(&f, "gitlab.com"), Some(true));
        assert_eq!(known_host(&f, "github.com"), Some(true));
        assert_eq!(known_host(&f, "example.org"), Some(false));
        std::fs::write(&f, "|1|abc=|def= ssh-ed25519 AAAA\n").unwrap();
        assert_eq!(known_host(&f, "github.com"), None);
        assert_eq!(known_host(&dir.join("missing"), "github.com"), Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn generation_and_upload_commands() {
        let p = Path::new("C:/Users/me/.ssh/id_ed25519");
        assert_eq!(
            cmd_line("ssh-keygen", &generate_args(p, "me@x.example", false)),
            "ssh-keygen -t ed25519 -C me@x.example -f C:/Users/me/.ssh/id_ed25519 -N \"\""
        );
        assert_eq!(
            terminal_command(p, "me@x.example"),
            "ssh-keygen -t ed25519 -C me@x.example -f C:/Users/me/.ssh/id_ed25519"
        );
        assert_eq!(
            cmd_line(
                "gh",
                &upload_args(Path::new("C:/k.pub"), KeyUse::Signing, "My laptop")
            ),
            "gh ssh-key add C:/k.pub --type signing --title \"My laptop\""
        );
        assert_eq!(KeyUse::Authentication.scope(), "admin:public_key");
        assert!(plan_generate("ssh-keygen", p, "").is_err());
    }

    #[test]
    fn allowed_signers_format() {
        assert_eq!(
            allowed_signers_line("me@x.example", "ssh-ed25519", "AAAAC3", "laptop"),
            "me@x.example namespaces=\"git\" ssh-ed25519 AAAAC3 laptop"
        );
        assert_eq!(
            allowed_signers_line(" me@x.example ", "ssh-ed25519", "AAAAC3", ""),
            "me@x.example namespaces=\"git\" ssh-ed25519 AAAAC3"
        );
    }

    #[test]
    fn keys_are_listed_with_their_fingerprints() {
        let dir = std::env::temp_dir().join(format!("gitvis-id-ssh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("id_ed25519.pub"), format!("{ED}\n")).unwrap();
        std::fs::write(dir.join("id_ed25519"), "private").unwrap();
        std::fs::write(dir.join("junk.pub"), "nothing").unwrap();
        let mut cmds = Vec::new();
        let keys = list(&dir, None, &mut cmds);
        assert_eq!(keys.len(), 1);
        let k = &keys[0];
        assert!(k.has_private);
        assert_eq!(k.kind(), "ED25519");
        assert_eq!(k.fingerprint.bits, Some(256));
        assert_eq!(k.name(), "id_ed25519.pub");
        assert!(k.is(&crate::devops::identity::slash(&k.private), None));
        assert!(k.is(&crate::devops::identity::slash(&k.file), None));
        assert!(k.is(&format!("key::{}", k.line()), None));
        assert!(!k.is("key::ssh-ed25519 AAAAother", None));
        if let Some(kg) = crate::devops::proc::resolve("ssh-keygen") {
            let with = list(&dir, Some(&kg.to_string_lossy()), &mut cmds);
            assert_eq!(with[0].fingerprint.hash, k.fingerprint.hash);
            assert!(cmds.iter().any(|c| c.contains("-lf")));
        }
        assert_eq!(next_key_path(&dir), dir.join("id_ed25519_2"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
