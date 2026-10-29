//! `SHA256SUMS` files and SHA-256 of downloads.
//!
//! Accepted line formats: GNU `sha256sum` (`<hex>  name`, `<hex> *name`
//! for binary mode) and BSD `shasum --tag` (`SHA256 (name) = <hex>`).
//! Comments (`#`) and blank lines are ignored.

use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

/// One `SHA256SUMS` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sum {
    /// Lower-case hex.
    pub hex: String,
    /// The file name as listed (a leading `./` removed).
    pub name: String,
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn clean_name(n: &str) -> String {
    n.trim().trim_start_matches("./").replace('\\', "/")
}

/// Parse a checksum file; malformed lines are skipped.
pub fn parse_sums(text: &str) -> Vec<Sum> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_start_matches('\u{feff}').trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("SHA256 (")
            && let Some((name, hex)) = rest.rsplit_once(") = ")
            && is_sha256_hex(hex.trim())
        {
            out.push(Sum {
                hex: hex.trim().to_ascii_lowercase(),
                name: clean_name(name),
            });
            continue;
        }
        let Some((hex, name)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        if !is_sha256_hex(hex) {
            continue;
        }
        let name = name.trim_start();
        let name = name.strip_prefix('*').unwrap_or(name);
        if name.is_empty() {
            continue;
        }
        out.push(Sum {
            hex: hex.to_ascii_lowercase(),
            name: clean_name(name),
        });
    }
    out
}

/// The expected hash of `name` (compared by base name, case-insensitively,
/// so `dist/hadion-….zip` in the file still matches the asset).
pub fn lookup<'a>(sums: &'a [Sum], name: &str) -> Option<&'a str> {
    let base = |s: &str| s.rsplit('/').next().unwrap_or(s).to_ascii_lowercase();
    let want = base(name);
    sums.iter()
        .find(|s| s.name.eq_ignore_ascii_case(name))
        .or_else(|| sums.iter().find(|s| base(&s.name) == want))
        .map(|s| s.hex.as_str())
}

/// Lower-case hex of a byte slice.
pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

pub fn sha256_bytes(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

/// Hash a file in 256 KiB chunks.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

/// The hex of GitHub's asset `digest` (`sha256:<hex>`), when it is SHA-256.
pub fn api_digest(digest: Option<&str>) -> Option<String> {
    let d = digest?.trim();
    let hex = d.strip_prefix("sha256:")?;
    is_sha256_hex(hex).then(|| hex.to_ascii_lowercase())
}

/// Check `actual` against the `SHA256SUMS` entry and, when GitHub gave one,
/// the API digest. Returns a description of what matched.
pub fn verify(
    sums_text: &str,
    asset: &str,
    actual: &str,
    digest: Option<&str>,
) -> Result<String, String> {
    let sums = parse_sums(sums_text);
    if sums.is_empty() {
        return Err("SHA256SUMS lists no checksums".to_owned());
    }
    let Some(expected) = lookup(&sums, asset) else {
        return Err(format!(
            "SHA256SUMS has no line for {asset} (it lists {})",
            sums.iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };
    if !expected.eq_ignore_ascii_case(actual) {
        return Err(format!(
            "Checksum mismatch for {asset}: SHA256SUMS says {expected}, the download is {actual}"
        ));
    }
    match api_digest(digest) {
        Some(d) if !d.eq_ignore_ascii_case(actual) => Err(format!(
            "Checksum mismatch for {asset}: GitHub's asset digest is {d}, the download is {actual}"
        )),
        Some(_) => Ok("matches SHA256SUMS and GitHub's asset digest".to_owned()),
        None => Ok("matches SHA256SUMS".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
    const B: &str = "60303ae22b998861bce3b28f33eec1be758a213c86c93c076dbe9f558c11c752";

    #[test]
    fn parses_gnu_bsd_and_binary_lines() {
        let text = format!(
            "\u{feff}# made by package.ps1\n{A}  hadion-0.2.0-windows-x86_64.zip\n{}\t*hadion-0.2.0-linux-x86_64.zip\nSHA256 (./dist/hadion.exe) = {B}\nnot a line\n{}  short\n\n",
            B.to_uppercase(),
            &A[..10]
        );
        let sums = parse_sums(&text);
        assert_eq!(sums.len(), 3);
        assert_eq!(sums[0].name, "hadion-0.2.0-windows-x86_64.zip");
        assert_eq!(sums[1].hex, B);
        assert_eq!(sums[1].name, "hadion-0.2.0-linux-x86_64.zip");
        assert_eq!(sums[2].name, "dist/hadion.exe");
        assert_eq!(lookup(&sums, "hadion.exe"), Some(B));
        assert_eq!(lookup(&sums, "HADION-0.2.0-WINDOWS-X86_64.ZIP"), Some(A));
        assert_eq!(lookup(&sums, "other.zip"), None);
    }

    #[test]
    fn hashes_and_verifies() {
        // sha256("test")
        assert_eq!(sha256_bytes(b"test"), A);
        let dir = std::env::temp_dir().join(format!("hadion-sum-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.zip");
        std::fs::write(&f, b"test").unwrap();
        assert_eq!(sha256_file(&f).unwrap(), A);
        let _ = std::fs::remove_dir_all(&dir);

        let sums = format!("{A}  a.zip\n");
        assert_eq!(
            verify(&sums, "a.zip", A, None).unwrap(),
            "matches SHA256SUMS"
        );
        assert!(
            verify(&sums, "a.zip", A, Some(&format!("sha256:{A}")))
                .unwrap()
                .contains("asset digest")
        );
        let e = verify(&sums, "a.zip", B, None).unwrap_err();
        assert!(
            e.contains("mismatch") && e.contains(A) && e.contains(B),
            "{e}"
        );
        let e = verify(&sums, "a.zip", A, Some(&format!("sha256:{B}"))).unwrap_err();
        assert!(e.contains("GitHub's asset digest"), "{e}");
        let e = verify(&sums, "b.zip", A, None).unwrap_err();
        assert!(
            e.contains("no line for b.zip") && e.contains("a.zip"),
            "{e}"
        );
        assert!(verify("", "a.zip", A, None).is_err());
        assert_eq!(api_digest(Some("md5:abc")), None);
    }
}
