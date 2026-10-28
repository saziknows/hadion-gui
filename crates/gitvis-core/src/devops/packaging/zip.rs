//! Just enough ZIP reading to take the executable out of a release archive:
//! the central directory, stored and deflated entries (through `flate2`),
//! CRC-32 and size checks. `osint::zip_entry` caps entries at 16 MB for
//! metadata parsing, which a release binary exceeds, hence this reader.
//!
//! Entry names are normalised to forward slashes: Windows PowerShell 5.1's
//! `Compress-Archive` writes backslashes.

use std::io::Read;

/// Largest archive and entry this reader accepts.
pub const MAX_SIZE: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Forward slashes.
    pub name: String,
    /// 0 stored, 8 deflated.
    pub method: u16,
    pub compressed: u64,
    pub size: u64,
    pub crc32: u32,
    local_offset: u64,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.name.ends_with('/')
    }

    /// The last path component.
    pub fn base_name(&self) -> &str {
        self.name
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("")
    }

    /// Directory depth (`gitvis.exe` 0, `gitvis-0.2.0/gitvis.exe` 1).
    pub fn depth(&self) -> usize {
        self.name.trim_end_matches('/').matches('/').count()
    }
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// The entries of an archive, from its central directory.
pub fn entries(data: &[u8]) -> Result<Vec<Entry>, String> {
    if data.len() < 22 {
        return Err("Not a ZIP archive (too short)".to_owned());
    }
    // The end-of-central-directory record is in the last 64 KiB + 22 bytes.
    let start = data.len().saturating_sub(65_557);
    let eocd = (start..=data.len() - 22)
        .rev()
        .find(|&i| data[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])
        .ok_or("Not a ZIP archive (no end of central directory)")?;
    let count = u16_at(data, eocd + 10).ok_or("Truncated ZIP directory")? as usize;
    let cd = u32_at(data, eocd + 16).ok_or("Truncated ZIP directory")? as usize;
    if count == 0xffff || cd == 0xffff_ffff {
        return Err("ZIP64 archives are not supported".to_owned());
    }
    let mut out = Vec::with_capacity(count);
    let mut pos = cd;
    for _ in 0..count {
        if data.get(pos..pos + 4) != Some(&[0x50, 0x4b, 0x01, 0x02]) {
            return Err(format!("Corrupt ZIP directory at byte {pos}"));
        }
        let field = |off: usize| u16_at(data, pos + off).ok_or("Truncated ZIP directory");
        let field32 = |off: usize| u32_at(data, pos + off).ok_or("Truncated ZIP directory");
        let method = field(10)?;
        let crc32 = field32(16)?;
        let compressed = field32(20)? as u64;
        let size = field32(24)? as u64;
        let name_len = field(28)? as usize;
        let extra_len = field(30)? as usize;
        let comment_len = field(32)? as usize;
        let local_offset = field32(42)? as u64;
        let name = data
            .get(pos + 46..pos + 46 + name_len)
            .ok_or("Truncated ZIP directory")?;
        out.push(Entry {
            name: String::from_utf8_lossy(name).replace('\\', "/"),
            method,
            compressed,
            size,
            crc32,
            local_offset,
        });
        pos += 46 + name_len + extra_len + comment_len;
    }
    Ok(out)
}

/// The entry for `file_name` (e.g. `gitvis.exe`): matched by base name,
/// case-insensitively; the shallowest one wins.
pub fn find<'a>(entries: &'a [Entry], file_name: &str) -> Option<&'a Entry> {
    entries
        .iter()
        .filter(|e| !e.is_dir() && e.base_name().eq_ignore_ascii_case(file_name))
        .min_by_key(|e| e.depth())
}

/// Decompress one entry, checking its size and CRC-32.
pub fn read(data: &[u8], e: &Entry) -> Result<Vec<u8>, String> {
    if e.size > MAX_SIZE || e.compressed > MAX_SIZE {
        return Err(format!("{} is too large ({} bytes)", e.name, e.size));
    }
    let local = e.local_offset as usize;
    if data.get(local..local + 4) != Some(&[0x50, 0x4b, 0x03, 0x04]) {
        return Err(format!("Corrupt ZIP entry {}", e.name));
    }
    let flags = u16_at(data, local + 6).ok_or("Truncated ZIP entry")?;
    if flags & 1 != 0 {
        return Err(format!("{} is encrypted", e.name));
    }
    let lname = u16_at(data, local + 26).ok_or("Truncated ZIP entry")? as usize;
    let lextra = u16_at(data, local + 28).ok_or("Truncated ZIP entry")? as usize;
    let begin = local + 30 + lname + lextra;
    let raw = data
        .get(begin..begin + e.compressed as usize)
        .ok_or_else(|| format!("{} is cut short (the archive is truncated)", e.name))?;
    let out = match e.method {
        0 => raw.to_vec(),
        8 => {
            let mut out = Vec::with_capacity(e.size as usize);
            flate2::read::DeflateDecoder::new(raw)
                .take(e.size + 1)
                .read_to_end(&mut out)
                .map_err(|err| format!("Could not decompress {}: {err}", e.name))?;
            out
        }
        m => {
            return Err(format!(
                "{} uses compression method {m}; only stored and deflated entries are supported",
                e.name
            ));
        }
    };
    if out.len() as u64 != e.size {
        return Err(format!(
            "{} decompressed to {} bytes, the archive says {}",
            e.name,
            out.len(),
            e.size
        ));
    }
    let mut crc = flate2::Crc::new();
    crc.update(&out);
    if crc.sum() != e.crc32 {
        return Err(format!(
            "CRC-32 mismatch in {} ({:08x}, the archive says {:08x})",
            e.name,
            crc.sum(),
            e.crc32
        ));
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod testing {
    //! A minimal ZIP writer for tests.
    use std::io::Write;

    pub fn build(files: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data, deflate) in files {
            let mut crc = flate2::Crc::new();
            crc.update(data);
            let body = if *deflate {
                let mut e =
                    flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
                e.write_all(data).unwrap();
                e.finish().unwrap()
            } else {
                data.to_vec()
            };
            let method: u16 = if *deflate { 8 } else { 0 };
            let offset = out.len() as u32;
            let header = |sig: u32, central_dir: bool| {
                let mut h = Vec::new();
                h.extend_from_slice(&sig.to_le_bytes());
                if central_dir {
                    h.extend_from_slice(&20u16.to_le_bytes()); // made by
                }
                h.extend_from_slice(&20u16.to_le_bytes()); // needed
                h.extend_from_slice(&0u16.to_le_bytes()); // flags
                h.extend_from_slice(&method.to_le_bytes());
                h.extend_from_slice(&0u32.to_le_bytes()); // time, date
                h.extend_from_slice(&crc.sum().to_le_bytes());
                h.extend_from_slice(&(body.len() as u32).to_le_bytes());
                h.extend_from_slice(&(data.len() as u32).to_le_bytes());
                h.extend_from_slice(&(name.len() as u16).to_le_bytes());
                h.extend_from_slice(&0u16.to_le_bytes()); // extra
                if central_dir {
                    h.extend_from_slice(&0u16.to_le_bytes()); // comment
                    h.extend_from_slice(&0u16.to_le_bytes()); // disk
                    h.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
                    h.extend_from_slice(&0u32.to_le_bytes()); // external attrs
                    h.extend_from_slice(&offset.to_le_bytes());
                }
                h.extend_from_slice(name.as_bytes());
                h
            };
            out.extend(header(0x0403_4b50, false));
            out.extend(&body);
            central.extend(header(0x0201_4b50, true));
        }
        let cd_offset = out.len() as u32;
        out.extend(&central);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_stored_and_deflated_entries() {
        let big: Vec<u8> = (0..200_000u32)
            .flat_map(|i| (i % 251).to_le_bytes())
            .collect();
        let zip = testing::build(&[
            ("gitvis-0.2.0\\README.md", b"read me", false),
            ("gitvis-0.2.0\\gitvis.exe", &big, true),
            ("gitvis-0.2.0/licenses/", b"", false),
        ]);
        let list = entries(&zip).unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[1].name, "gitvis-0.2.0/gitvis.exe");
        assert_eq!(list[1].base_name(), "gitvis.exe");
        assert_eq!(list[1].depth(), 1);
        assert!(list[2].is_dir());
        let e = find(&list, "GITVIS.EXE").unwrap();
        assert_eq!(read(&zip, e).unwrap(), big);
        assert_eq!(read(&zip, &list[0]).unwrap(), b"read me");
        assert!(find(&list, "gitvis").is_none());
    }

    #[test]
    fn shallowest_match_wins() {
        let zip = testing::build(&[
            ("a/b/gitvis.exe", b"deep", false),
            ("a/gitvis.exe", b"top", false),
        ]);
        let list = entries(&zip).unwrap();
        assert_eq!(
            read(&zip, find(&list, "gitvis.exe").unwrap()).unwrap(),
            b"top"
        );
    }

    #[test]
    fn rejects_corruption() {
        assert!(entries(b"not a zip at all, definitely not").is_err());
        assert!(entries(b"").is_err());
        let mut zip = testing::build(&[("gitvis.exe", b"hello world hello world", true)]);
        let list = entries(&zip).unwrap();
        // Flip a byte inside the compressed data: CRC (or inflate) fails.
        zip[40] ^= 0xff;
        assert!(read(&zip, &list[0]).is_err());
        // Truncated archive: the directory is gone.
        let zip = testing::build(&[("gitvis.exe", b"x", false)]);
        assert!(entries(&zip[..zip.len() - 30]).is_err());
    }
}
