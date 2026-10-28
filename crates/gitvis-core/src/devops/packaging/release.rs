//! GitHub releases and the release contract gitvis updates from.
//!
//! A release is usable when its tag is a version (`v0.2.0`) and it carries
//! `gitvis-<version>-<os>-<arch>.zip` (or, on Windows, the bare `.exe`) plus
//! a `SHA256SUMS` file listing that asset. The test format
//! (`GITVIS_UPDATE_DIR/releases.json`) is the same JSON GitHub answers for
//! `GET repos/{o}/{r}/releases`, or that array under a `releases` key.

use serde::{Deserialize, Serialize};

use super::version::Version;

/// The checksum file every release carries.
pub const SUMS: &str = "SHA256SUMS";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Asset {
    pub name: String,
    pub size: u64,
    pub browser_download_url: String,
    pub content_type: String,
    /// `sha256:<hex>`, which GitHub computes for assets uploaded since 2025.
    pub digest: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Release {
    pub tag_name: String,
    pub name: Option<String>,
    pub body: Option<String>,
    pub draft: bool,
    pub prerelease: bool,
    pub published_at: Option<String>,
    pub html_url: String,
    pub assets: Vec<Asset>,
}

impl Release {
    pub fn version(&self) -> Option<Version> {
        Version::parse(&self.tag_name)
    }

    /// The release's name, or its tag when it has none.
    pub fn title(&self) -> String {
        match self.name.as_deref().map(str::trim) {
            Some(n) if !n.is_empty() => n.to_owned(),
            _ => self.tag_name.clone(),
        }
    }

    /// Marked as a pre-release on GitHub, or a pre-release version.
    pub fn is_prerelease(&self) -> bool {
        self.prerelease || self.version().is_some_and(|v| v.is_prerelease())
    }

    pub fn notes(&self) -> &str {
        self.body.as_deref().unwrap_or("").trim()
    }

    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(name))
    }
}

/// Which releases count.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Channel {
    #[default]
    Stable,
    /// Stable releases and pre-releases.
    PreRelease,
}

impl Channel {
    pub fn label(self) -> &'static str {
        match self {
            Channel::Stable => "Stable releases",
            Channel::PreRelease => "Pre-releases too",
        }
    }

    pub fn admits(self, r: &Release) -> bool {
        match self {
            Channel::Stable => !r.is_prerelease(),
            Channel::PreRelease => true,
        }
    }
}

/// Operating system and architecture in asset names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Platform {
    /// `windows`, `linux`, `macos`.
    pub os: String,
    /// `x86_64`, `aarch64`.
    pub arch: String,
}

impl Platform {
    pub fn new(os: &str, arch: &str) -> Self {
        Self {
            os: os.to_owned(),
            arch: arch.to_owned(),
        }
    }

    /// The platform this binary was built for.
    pub fn current() -> Self {
        Self::new(std::env::consts::OS, std::env::consts::ARCH)
    }

    /// `windows-x86_64`.
    pub fn label(&self) -> String {
        format!("{}-{}", self.os, self.arch)
    }

    /// The executable's file name inside an archive.
    pub fn exe_name(&self) -> &'static str {
        if self.os == "windows" {
            "gitvis.exe"
        } else {
            "gitvis"
        }
    }

    /// Asset names in order of preference: the zip, then the bare
    /// executable (Windows only), each also with a `v` before the version.
    pub fn asset_names(&self, version: &str) -> Vec<String> {
        let version = version.trim_start_matches(['v', 'V']);
        let mut out = Vec::new();
        for v in [version.to_owned(), format!("v{version}")] {
            out.push(format!("gitvis-{v}-{}.zip", self.label()));
            if self.os == "windows" {
                out.push(format!("gitvis-{v}-{}.exe", self.label()));
            }
        }
        out
    }

    /// This platform's asset of a release.
    pub fn select<'a>(&self, r: &'a Release) -> Option<&'a Asset> {
        let version = r
            .version()
            .map(|v| v.plain())
            .unwrap_or_else(|| r.tag_name.clone());
        self.asset_names(&version).iter().find_map(|n| r.asset(n))
    }
}

/// Is the asset a zip archive (as opposed to a bare executable)?
pub fn is_zip(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".zip")
}

/// Decode a release list: GitHub's array, or `{"releases": [...]}`.
pub fn parse_releases(json: &[u8]) -> Result<Vec<Release>, String> {
    let v: serde_json::Value =
        serde_json::from_slice(json).map_err(|e| format!("Not valid JSON: {e}"))?;
    let list = match v {
        serde_json::Value::Array(_) => v,
        serde_json::Value::Object(mut m) => match m.remove("releases") {
            Some(list @ serde_json::Value::Array(_)) => list,
            _ => {
                // GitHub's error shape: {"message": "Not Found", ...}
                let msg = m
                    .get("message")
                    .and_then(|x| x.as_str())
                    .unwrap_or("expected an array of releases");
                return Err(msg.to_owned());
            }
        },
        _ => return Err("expected an array of releases".to_owned()),
    };
    serde_json::from_value(list).map_err(|e| format!("Unexpected release format: {e}"))
}

/// A release newer than the running build, with what an update needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub release: Release,
    pub version: Version,
    /// This platform's asset, when the release has one.
    pub asset: Option<Asset>,
    /// The `SHA256SUMS` asset, when the release has one.
    pub sums: Option<Asset>,
}

impl Candidate {
    /// Why it cannot be installed, or `None` when it can.
    pub fn problem(&self, platform: &Platform) -> Option<String> {
        if self.asset.is_none() {
            // The names without a `v`: the zip, and the exe on Windows.
            let names = platform.asset_names(&self.version.plain());
            let n = if platform.os == "windows" { 2 } else { 1 };
            return Some(format!(
                "{} has no asset for {} (expected {})",
                self.release.tag_name,
                platform.label(),
                names[..n.min(names.len())].join(" or ")
            ));
        }
        if self.sums.is_none() {
            return Some(format!(
                "{} has no {SUMS} file, so the download cannot be verified",
                self.release.tag_name
            ));
        }
        None
    }
}

/// What a check found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evaluation {
    pub current: Version,
    /// Releases in the channel newer than the current build, newest first.
    pub newer: Vec<Candidate>,
    /// The newest release in the channel (may be the running one or older).
    pub latest: Option<(Release, Version)>,
    /// Releases looked at (drafts and other-channel ones included).
    pub seen: usize,
    /// Tags that are not versions, drafts and releases outside the channel.
    pub skipped: Vec<String>,
}

impl Evaluation {
    /// The release to install: the newest candidate.
    pub fn best(&self) -> Option<&Candidate> {
        self.newer.first()
    }

    /// The running build is newer than every release in the channel.
    pub fn ahead(&self) -> bool {
        self.latest.as_ref().is_some_and(|(_, v)| *v < self.current)
    }
}

/// Compare a release list with the running version.
pub fn evaluate(
    releases: &[Release],
    current: &Version,
    channel: Channel,
    platform: &Platform,
) -> Evaluation {
    let mut skipped = Vec::new();
    let mut usable: Vec<(Release, Version)> = Vec::new();
    for r in releases {
        if r.draft {
            skipped.push(format!("{} (draft)", r.tag_name));
            continue;
        }
        let Some(v) = r.version() else {
            skipped.push(format!("{} (not a version)", r.tag_name));
            continue;
        };
        if !channel.admits(r) {
            skipped.push(format!("{} (pre-release)", r.tag_name));
            continue;
        }
        usable.push((r.clone(), v));
    }
    usable.sort_by(|a, b| b.1.cmp(&a.1));
    let latest = usable.first().cloned();
    let newer = usable
        .into_iter()
        .filter(|(_, v)| v > current)
        .map(|(r, v)| Candidate {
            asset: platform.select(&r).cloned(),
            sums: r.asset(SUMS).cloned(),
            release: r,
            version: v,
        })
        .collect();
    Evaluation {
        current: current.clone(),
        newer,
        latest,
        seen: releases.len(),
        skipped,
    }
}

/// `owner/repo` with GitHub's allowed characters.
pub fn valid_repo(s: &str) -> bool {
    let Some((o, r)) = s.trim().split_once('/') else {
        return false;
    };
    let ok = |p: &str| {
        !p.is_empty()
            && p.len() <= 100
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    ok(o) && ok(r) && r != "." && r != ".."
}

/// The release list endpoint (newest ten, drafts included for maintainers).
pub fn releases_endpoint(repo: &str) -> String {
    format!("repos/{}/releases?per_page=10", repo.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> Asset {
        Asset {
            name: name.into(),
            size: 10,
            ..Default::default()
        }
    }

    fn rel(tag: &str, pre: bool, assets: &[&str]) -> Release {
        Release {
            tag_name: tag.into(),
            prerelease: pre,
            assets: assets.iter().map(|n| asset(n)).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn asset_selection_per_os_and_arch() {
        let r = rel(
            "v0.2.0",
            false,
            &[
                "gitvis-0.2.0-linux-x86_64.zip",
                "gitvis-0.2.0-windows-x86_64.exe",
                "gitvis-0.2.0-windows-x86_64.zip",
                "gitvis-0.2.0-macos-aarch64.zip",
                "SHA256SUMS",
            ],
        );
        let win = Platform::new("windows", "x86_64");
        assert_eq!(
            win.select(&r).unwrap().name,
            "gitvis-0.2.0-windows-x86_64.zip"
        );
        assert_eq!(
            Platform::new("linux", "x86_64").select(&r).unwrap().name,
            "gitvis-0.2.0-linux-x86_64.zip"
        );
        assert_eq!(
            Platform::new("macos", "aarch64").select(&r).unwrap().name,
            "gitvis-0.2.0-macos-aarch64.zip"
        );
        assert!(Platform::new("windows", "aarch64").select(&r).is_none());
        assert!(Platform::new("macos", "x86_64").select(&r).is_none());
        // Only the bare exe: Windows takes it, Linux has no `.exe` fallback.
        let exe_only = rel(
            "v0.3.0-rc.1",
            true,
            &["gitvis-0.3.0-rc.1-windows-x86_64.exe"],
        );
        assert_eq!(
            win.select(&exe_only).unwrap().name,
            "gitvis-0.3.0-rc.1-windows-x86_64.exe"
        );
        let linux_exe = rel("v0.3.0", false, &["gitvis-0.3.0-linux-x86_64.exe"]);
        assert!(
            Platform::new("linux", "x86_64")
                .select(&linux_exe)
                .is_none()
        );
        // A `v` in the asset name and odd case are tolerated.
        let v_named = rel("0.4.0", false, &["GITVIS-v0.4.0-windows-x86_64.ZIP"]);
        assert!(win.select(&v_named).is_some());
        assert!(is_zip("GITVIS-v0.4.0-windows-x86_64.ZIP"));
        assert_eq!(win.exe_name(), "gitvis.exe");
        assert_eq!(Platform::new("linux", "x86_64").exe_name(), "gitvis");
    }

    #[test]
    fn evaluation_by_channel() {
        let list = vec![
            rel("v0.3.0-beta.1", true, &[]),
            rel(
                "v0.2.0",
                false,
                &["gitvis-0.2.0-windows-x86_64.zip", "SHA256SUMS"],
            ),
            rel("v0.1.1", false, &["gitvis-0.1.1-windows-x86_64.zip"]),
            rel("v0.1.0", false, &[]),
            rel("nightly", true, &[]),
            Release {
                draft: true,
                ..rel("v9.0.0", false, &[])
            },
        ];
        let win = Platform::new("windows", "x86_64");
        let current = Version::parse("0.1.0+051465f").unwrap();
        let stable = evaluate(&list, &current, Channel::Stable, &win);
        let tags: Vec<&str> = stable
            .newer
            .iter()
            .map(|c| c.release.tag_name.as_str())
            .collect();
        assert_eq!(tags, ["v0.2.0", "v0.1.1"]);
        assert_eq!(stable.best().unwrap().problem(&win), None);
        let p = stable.newer[1].problem(&win).unwrap();
        assert!(p.contains("no SHA256SUMS"), "{p}");
        assert_eq!(stable.latest.as_ref().unwrap().0.tag_name, "v0.2.0");
        assert_eq!(stable.seen, 6);
        assert!(stable.skipped.iter().any(|s| s == "v9.0.0 (draft)"));
        assert!(
            stable
                .skipped
                .iter()
                .any(|s| s == "nightly (not a version)")
        );
        assert!(!stable.ahead());

        let pre = evaluate(&list, &current, Channel::PreRelease, &win);
        assert_eq!(pre.best().unwrap().release.tag_name, "v0.3.0-beta.1");
        let p = pre.best().unwrap().problem(&win).unwrap();
        assert!(p.contains("no asset for windows-x86_64"), "{p}");
        assert!(p.contains("gitvis-0.3.0-beta.1-windows-x86_64.zip"), "{p}");

        // Up to date and ahead.
        let at = Version::parse("0.2.0").unwrap();
        let e = evaluate(&list, &at, Channel::Stable, &win);
        assert!(e.newer.is_empty() && !e.ahead());
        let dev = Version::parse("0.5.0").unwrap();
        assert!(evaluate(&list, &dev, Channel::Stable, &win).ahead());
        assert!(evaluate(&[], &dev, Channel::Stable, &win).latest.is_none());
    }

    #[test]
    fn releases_json_formats() {
        let github = br#"[{"tag_name":"v0.2.0","name":"Second","body":"Notes","prerelease":false,
            "draft":false,"published_at":"2026-10-20T10:00:00Z","html_url":"https://github.com/o/r/releases/tag/v0.2.0",
            "assets":[{"name":"SHA256SUMS","size":120,"browser_download_url":"https://x","digest":"sha256:ab"}],
            "author":{"login":"someone"}}]"#;
        let r = parse_releases(github).unwrap();
        assert_eq!(r[0].title(), "Second");
        assert_eq!(r[0].notes(), "Notes");
        assert_eq!(r[0].assets[0].digest.as_deref(), Some("sha256:ab"));
        let wrapped = br#"{"releases":[{"tag_name":"v1.0.0","assets":[]}]}"#;
        let r = parse_releases(wrapped).unwrap();
        assert_eq!(r[0].title(), "v1.0.0");
        assert!(!r[0].prerelease);
        assert_eq!(
            parse_releases(br#"{"message":"Not Found","status":"404"}"#).unwrap_err(),
            "Not Found"
        );
        assert!(
            parse_releases(b"<html>")
                .unwrap_err()
                .starts_with("Not valid JSON")
        );
    }

    #[test]
    fn repo_names() {
        assert!(valid_repo("saziknows/hadion-gui"));
        assert!(valid_repo(" o/r.rs "));
        for bad in ["", "o", "o/", "/r", "o/r/x", "o r/x", "o/..", "o/r?x=1"] {
            assert!(!valid_repo(bad), "{bad}");
        }
        assert_eq!(releases_endpoint("o/r"), "repos/o/r/releases?per_page=10");
    }
}
