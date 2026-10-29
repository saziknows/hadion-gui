//! GnuPG keys: where gpg is (Git for Windows ships one that is usually not
//! on PATH), its home folder, and the secret keys
//! (`gpg --list-secret-keys --keyid-format=long --with-colons`).
//!
//! The listing passes `--trust-model always --no-auto-check-trustdb` so
//! that reading never creates or rebuilds a trust database; the home folder
//! is not touched at all when it does not exist.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::tool;

/// A user id of a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Uid {
    pub text: String,
    pub validity: String,
}

/// A subkey (`ssb`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubKey {
    pub keyid: String,
    pub algo: String,
    pub caps: String,
    pub expires: Option<i64>,
    pub validity: String,
}

/// A secret key (`sec`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    /// Long key id (16 hex digits), what `user.signingkey` holds.
    pub keyid: String,
    pub fingerprint: String,
    /// `ed25519`, `rsa3072`.
    pub algo: String,
    pub created: Option<i64>,
    pub expires: Option<i64>,
    /// Field 2 of the record: `e` expired, `r` revoked, `d` disabled, …
    pub validity: String,
    /// Field 12: lower case for this key, upper case for the key with its
    /// subkeys (`scSC`).
    pub caps: String,
    pub uids: Vec<Uid>,
    pub subkeys: Vec<SubKey>,
}

impl Key {
    pub fn revoked(&self) -> bool {
        self.validity.contains('r')
    }

    pub fn expired(&self, now: i64) -> bool {
        self.validity.contains('e') || self.expires.is_some_and(|e| e <= now)
    }

    /// Can the key (or one of its subkeys) sign?
    pub fn can_sign(&self) -> bool {
        self.caps.contains('S')
    }

    /// `Sign`, `Certify`, `Encrypt`, `Authenticate` for the whole key.
    pub fn capabilities(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        for (c, label) in [
            ('S', "Sign"),
            ('C', "Certify"),
            ('E', "Encrypt"),
            ('A', "Authenticate"),
        ] {
            if self.caps.contains(c) {
                v.push(label);
            }
        }
        v
    }

    /// The first user id.
    pub fn primary_uid(&self) -> &str {
        self.uids.first().map(|u| u.text.as_str()).unwrap_or("")
    }

    /// The e-mail of the first user id that has one.
    pub fn email(&self) -> Option<String> {
        self.uids.iter().find_map(|u| {
            let s = u.text.find('<')?;
            let e = u.text[s..].find('>')?;
            Some(u.text[s + 1..s + e].to_owned())
        })
    }
}

/// gpg escapes `:` and other bytes as `\xNN` in user ids.
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && b.get(i + 1) == Some(&b'x')
            && let Some(h) = s.get(i + 2..i + 4)
            && let Ok(v) = u8::from_str_radix(h, 16)
        {
            out.push(v);
            i += 4;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `rsa3072`, `ed25519`, `dsa2048` from the algorithm number, size and
/// curve fields.
pub fn algo_name(code: &str, bits: &str, curve: &str) -> String {
    match code {
        "1" | "2" | "3" => format!("rsa{bits}"),
        "16" | "20" => format!("elg{bits}"),
        "17" => format!("dsa{bits}"),
        "18" | "19" | "22" if !curve.is_empty() => curve.to_owned(),
        "18" => "ecdh".to_owned(),
        "19" => "ecdsa".to_owned(),
        "22" => "eddsa".to_owned(),
        other => format!("algorithm {other}"),
    }
}

fn time(s: &str) -> Option<i64> {
    s.parse::<i64>().ok().filter(|t| *t > 0)
}

/// Parse `--with-colons` output of `--list-secret-keys`.
pub fn parse_colons(text: &str) -> Vec<Key> {
    let mut keys: Vec<Key> = Vec::new();
    // Which record the next `fpr` belongs to: the key or a subkey.
    let mut last_sub = false;
    for line in text.lines() {
        let f: Vec<&str> = line.split(':').collect();
        let get = |i: usize| f.get(i).copied().unwrap_or("");
        match get(0) {
            "sec" => {
                last_sub = false;
                keys.push(Key {
                    keyid: get(4).to_owned(),
                    fingerprint: String::new(),
                    algo: algo_name(get(3), get(2), get(16)),
                    created: time(get(5)),
                    expires: time(get(6)),
                    validity: get(1).to_owned(),
                    caps: get(11).to_owned(),
                    uids: Vec::new(),
                    subkeys: Vec::new(),
                });
            }
            "ssb" => {
                last_sub = true;
                if let Some(k) = keys.last_mut() {
                    k.subkeys.push(SubKey {
                        keyid: get(4).to_owned(),
                        algo: algo_name(get(3), get(2), get(16)),
                        caps: get(11).to_owned(),
                        expires: time(get(6)),
                        validity: get(1).to_owned(),
                    });
                }
            }
            "fpr" if !last_sub => {
                if let Some(k) = keys.last_mut()
                    && k.fingerprint.is_empty()
                {
                    k.fingerprint = get(9).to_owned();
                }
            }
            "uid" => {
                if let Some(k) = keys.last_mut() {
                    k.uids.push(Uid {
                        text: unescape(get(9)),
                        validity: get(1).to_owned(),
                    });
                }
            }
            _ => {}
        }
    }
    keys
}

/// Where gpg was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub program: PathBuf,
    /// `HADION_GPG`, `gpg.program`, `PATH` or `Git for Windows`.
    pub how: &'static str,
}

/// Find gpg: `HADION_GPG` (tests; a name that does not exist means
/// missing), the `gpg.program` setting, `gpg` on PATH, then the copy Git
/// for Windows ships in `usr/bin`.
pub fn locate(gpg_program: Option<&str>) -> Option<Found> {
    use crate::devops::proc::resolve;
    if let Ok(v) = std::env::var("HADION_GPG")
        && !v.trim().is_empty()
    {
        return resolve(v.trim()).map(|p| Found {
            program: p,
            how: "HADION_GPG",
        });
    }
    if let Some(p) = gpg_program.map(str::trim).filter(|p| !p.is_empty())
        && let Some(found) = resolve(&super::from_msys(p))
    {
        return Some(Found {
            program: found,
            how: "gpg.program",
        });
    }
    if let Some(p) = resolve("gpg") {
        return Some(Found {
            program: p,
            how: "PATH",
        });
    }
    let git = resolve("git")?;
    // <Git>/cmd/git.exe or <Git>/bin/git.exe or <Git>/mingw64/bin/git.exe
    for up in git.ancestors().skip(2).take(2) {
        let candidate = up.join("usr").join("bin").join("gpg.exe");
        if candidate.is_file() {
            return Some(Found {
                program: candidate,
                how: "Git for Windows",
            });
        }
    }
    None
}

/// `(version line, home folder)` from `gpg --version`.
pub fn parse_version(text: &str) -> (Option<String>, Option<String>) {
    let version = text.lines().next().map(|l| l.trim().to_owned());
    let home = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("Home:"))
        .map(|h| super::from_msys(h.trim()));
    (version.filter(|v| !v.is_empty()), home)
}

/// The listing arguments.
pub fn list_args() -> Vec<String> {
    [
        "--batch",
        "--no-tty",
        "--trust-model",
        "always",
        "--no-auto-check-trustdb",
        "--list-secret-keys",
        "--keyid-format=long",
        "--with-colons",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect()
}

/// gpg and its secret keys.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// `None`: gpg is not installed.
    pub found: Option<Found>,
    pub version: Option<String>,
    pub home: Option<String>,
    /// The home folder exists (otherwise there are no keys yet).
    pub home_exists: bool,
    pub keys: Vec<Key>,
    pub error: Option<String>,
}

/// Find gpg and list its secret keys.
pub fn load(dir: &Path, gpg_program: Option<&str>, cmds: &mut Vec<String>) -> Report {
    let Some(found) = locate(gpg_program) else {
        return Report::default();
    };
    let program = found.program.to_string_lossy().into_owned();
    let mut r = Report {
        found: Some(found),
        ..Default::default()
    };
    match tool(
        &program,
        &["--version".to_owned()],
        dir,
        Duration::from_secs(10),
        cmds,
    ) {
        Ok(c) if c.success() => {
            let (v, h) = parse_version(&c.stdout_text());
            r.version = v;
            r.home = h;
        }
        Ok(c) => {
            r.error = Some(c.error_text());
            return r;
        }
        Err(e) => {
            r.error = Some(e);
            return r;
        }
    }
    // An msys-only path (`/tmp/...`, `/home/...`) cannot be checked from here:
    // let gpg decide.
    r.home_exists = r.home.as_deref().is_none_or(|h| {
        Path::new(h).is_dir() || (h.starts_with('/') && !Path::new(h).is_absolute())
    });
    if !r.home_exists {
        return r;
    }
    match tool(&program, &list_args(), dir, Duration::from_secs(20), cmds) {
        Ok(c) if c.success() => r.keys = parse_colons(&c.stdout_text()),
        Ok(c) => r.error = Some(c.error_text()),
        Err(e) => r.error = Some(e),
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
sec:-:255:22:AE372EBC5B366C6A:1793137433:1824673433::u:::scSC:::+::ed25519:::0:
fpr:::::::::52134834A185E6BBC7701AC0AE372EBC5B366C6A:
grp:::::::::4DA4216DD7CEB40F3D75714F73DF145EC0EF3155:
uid:-::::1793137433::F7D0F78186AF699041C3DF5C9E3FE90AE496BAC1::Test Person <test@example.com>::::::::::0:
sec:e:2048:1:2E0026F96CB6ABDF:1577836800:1609372800::u:::sc:::+:::23::0:
fpr:::::::::DEFAEE456E12B3999F77B0B42E0026F96CB6ABDF:
uid:e::::1577836800::5626::Old Key (laptop\\x3a home) <old@example.com>::::::::::0:
sec:u:3072:1:B91FBCB5172A8901:1793137445:::u:::scESCA:::+:::23::0:
fpr:::::::::C0472541F10EF554AD5EA2B1B91FBCB5172A8901:
uid:u::::1793137445::6A60::Work Person <work@corp.example>::::::::::0:
uid:u::::1793137445::6A61::Work Person <alias@corp.example>::::::::::0:
ssb:u:3072:1:1111222233334444:1793137445::::::e:::+:::23:
fpr:::::::::AAAABBBBCCCCDDDDEEEEFFFF1111222233334444:
";

    #[test]
    fn colons_are_parsed() {
        let k = parse_colons(SAMPLE);
        assert_eq!(k.len(), 3);
        assert_eq!(k[0].keyid, "AE372EBC5B366C6A");
        assert_eq!(k[0].fingerprint, "52134834A185E6BBC7701AC0AE372EBC5B366C6A");
        assert_eq!(k[0].algo, "ed25519");
        assert_eq!(k[0].created, Some(1793137433));
        assert_eq!(k[0].expires, Some(1824673433));
        assert!(k[0].can_sign());
        assert_eq!(k[0].capabilities(), ["Sign", "Certify"]);
        assert_eq!(k[0].email().as_deref(), Some("test@example.com"));
        // Expired, with an escaped colon in the uid.
        assert!(k[1].expired(1_800_000_000));
        assert_eq!(k[1].algo, "rsa2048");
        assert_eq!(
            k[1].primary_uid(),
            "Old Key (laptop: home) <old@example.com>"
        );
        // Subkey fingerprints do not replace the key's.
        assert_eq!(k[2].fingerprint, "C0472541F10EF554AD5EA2B1B91FBCB5172A8901");
        assert_eq!(k[2].uids.len(), 2);
        assert_eq!(k[2].subkeys.len(), 1);
        assert_eq!(k[2].subkeys[0].caps, "e");
        assert_eq!(k[2].expires, None);
        assert!(!k[2].expired(i64::MAX - 1));
        assert_eq!(
            k[2].capabilities(),
            ["Sign", "Certify", "Encrypt", "Authenticate"]
        );
        assert!(parse_colons("").is_empty());
    }

    #[test]
    fn versions_and_homes() {
        let (v, h) = parse_version(
            "gpg (GnuPG) 2.4.9\nlibgcrypt 1.12.2\nCopyright\n\nHome: /c/Users/me/.gnupg\nSupported algorithms:\n",
        );
        assert_eq!(v.as_deref(), Some("gpg (GnuPG) 2.4.9"));
        if cfg!(windows) {
            assert_eq!(h.as_deref(), Some("C:/Users/me/.gnupg"));
        }
        assert_eq!(algo_name("19", "256", "nistp256"), "nistp256");
        assert_eq!(algo_name("17", "1024", ""), "dsa1024");
        assert!(list_args().contains(&"--with-colons".to_owned()));
    }
}
