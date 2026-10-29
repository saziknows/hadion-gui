//! What a build knows about itself. The app's `build.rs` embeds the git
//! commit, a dirty flag, the build date, the target, the profile, the
//! compiler and the GitHub remote as environment variables; any of them can
//! be missing (a source archive without `.git`, no git installed), so every
//! field has a fallback.

use super::version::Version;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildInfo {
    /// `CARGO_PKG_VERSION`.
    pub version: String,
    /// Full commit hash, empty when unknown.
    pub commit: String,
    /// Uncommitted changes to tracked files when it was built.
    pub dirty: bool,
    /// `2026-10-27T21:14:03Z`, empty when unknown.
    pub date: String,
    /// Target triple, e.g. `x86_64-pc-windows-gnu`.
    pub target: String,
    /// `debug` or `release`.
    pub profile: String,
    /// `rustc 1.97.1 (8bab26f4f 2026-07-14)`, empty when unknown.
    pub rustc: String,
    /// `owner/repo` of the GitHub `origin` it was built from, empty when
    /// there is none.
    pub remote: String,
}

fn clean(v: Option<&str>) -> String {
    v.map(str::trim).unwrap_or_default().to_owned()
}

/// The values `build.rs` embeds, by name, as `option_env!` gives them.
#[derive(Clone, Copy, Debug, Default)]
pub struct Embedded<'a> {
    pub version: &'a str,
    pub commit: Option<&'a str>,
    pub dirty: Option<&'a str>,
    pub date: Option<&'a str>,
    pub target: Option<&'a str>,
    pub profile: Option<&'a str>,
    pub rustc: Option<&'a str>,
    pub remote: Option<&'a str>,
}

impl BuildInfo {
    /// Build the info from embedded values, filling the gaps: the target
    /// from the running OS and architecture, the profile from
    /// `debug_assertions`.
    pub fn from_embedded(e: Embedded) -> Self {
        let commit: String = clean(e.commit)
            .chars()
            .filter(char::is_ascii_hexdigit)
            .collect();
        let target = clean(e.target);
        let profile = clean(e.profile);
        BuildInfo {
            version: if e.version.trim().is_empty() {
                "0.0.0".to_owned()
            } else {
                e.version.trim().to_owned()
            },
            commit: commit.to_ascii_lowercase(),
            dirty: matches!(clean(e.dirty).as_str(), "1" | "true" | "yes"),
            date: clean(e.date),
            target: if target.is_empty() {
                format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
            } else {
                target
            },
            profile: if profile.is_empty() {
                if cfg!(debug_assertions) {
                    "debug".to_owned()
                } else {
                    "release".to_owned()
                }
            } else {
                profile
            },
            rustc: clean(e.rustc),
            remote: clean(e.remote),
        }
    }

    /// First 7 characters of the commit, or `unknown`.
    pub fn short_commit(&self) -> String {
        if self.commit.is_empty() {
            "unknown".to_owned()
        } else {
            self.commit.chars().take(7).collect()
        }
    }

    /// `051465f`, `051465f (dirty)` or `unknown`.
    pub fn commit_label(&self) -> String {
        let mut s = self.short_commit();
        if self.dirty && !self.commit.is_empty() {
            s.push_str(" (dirty)");
        }
        s
    }

    /// `2026-10-27 21:14 UTC` or `unknown`.
    pub fn date_label(&self) -> String {
        match super::time::parse_utc(&self.date) {
            Some(t) => super::time::human(t),
            None if self.date.is_empty() => "unknown".to_owned(),
            None => self.date.clone(),
        }
    }

    /// The running version for update comparisons, the commit as build
    /// metadata (`0.1.0+051465f`). Unparseable versions count as `0.0.0`,
    /// so any release looks newer.
    pub fn semver(&self) -> Version {
        let mut v = Version::parse(&self.version).unwrap_or_else(|| Version::new(0, 0, 0));
        if v.build.is_empty() && !self.commit.is_empty() {
            v.build = self.short_commit();
        }
        v
    }

    /// `hadion 0.1.0 (051465f, dirty) x86_64-pc-windows-gnu release`, one
    /// line for reports and the window title.
    pub fn one_line(&self) -> String {
        let mut commit = self.short_commit();
        if self.dirty && !self.commit.is_empty() {
            commit.push_str(", dirty");
        }
        format!(
            "Hadion {} ({commit}) {} {}",
            self.version, self.target, self.profile
        )
    }
}

/// `owner/repo` of a GitHub remote URL (https, ssh or scp form), or `None`
/// for other hosts. Credentials in the URL are never part of the result.
pub fn github_repo_of_url(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = if let Some(r) = url.strip_prefix("git@github.com:") {
        r
    } else {
        let after_scheme = url.split_once("://").map(|(_, r)| r)?;
        // Drop `user:token@`.
        let host_path = after_scheme
            .rsplit_once('@')
            .map_or(after_scheme, |(_, r)| r);
        let (host, path) = host_path.split_once('/')?;
        let host = host.split(':').next().unwrap_or(host);
        if !host.eq_ignore_ascii_case("github.com") && !host.eq_ignore_ascii_case("www.github.com")
        {
            return None;
        }
        path
    };
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let mut parts = rest.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    (ok(owner) && ok(repo)).then(|| format!("{owner}/{repo}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallbacks_when_nothing_was_embedded() {
        let b = BuildInfo::from_embedded(Embedded {
            version: "0.1.0",
            ..Default::default()
        });
        assert_eq!(b.commit, "");
        assert_eq!(b.short_commit(), "unknown");
        assert_eq!(b.commit_label(), "unknown");
        assert!(!b.dirty);
        assert_eq!(b.date_label(), "unknown");
        assert!(b.target.contains(std::env::consts::OS));
        assert!(b.profile == "debug" || b.profile == "release");
        assert_eq!(b.semver().to_string(), "0.1.0");
        // Empty strings (build.rs outside a git checkout) are the same as None.
        let e = BuildInfo::from_embedded(Embedded {
            version: "",
            commit: Some(""),
            dirty: Some(""),
            date: Some(" "),
            target: Some(""),
            profile: Some(""),
            rustc: Some(""),
            remote: Some(""),
        });
        assert_eq!(e.version, "0.0.0");
        assert_eq!(e.short_commit(), "unknown");
        assert!(!e.target.is_empty() && !e.profile.is_empty());
    }

    #[test]
    fn embedded_values() {
        let b = BuildInfo::from_embedded(Embedded {
            version: "0.1.0",
            commit: Some("051465F7c0ffee\n"),
            dirty: Some("1"),
            date: Some("2026-10-27T21:14:03Z"),
            target: Some("x86_64-pc-windows-gnu"),
            profile: Some("release"),
            rustc: Some("rustc 1.97.1"),
            remote: Some("saziknows/hadion-gui"),
        });
        assert_eq!(b.short_commit(), "051465f");
        assert_eq!(b.commit_label(), "051465f (dirty)");
        assert_eq!(b.date_label(), "2026-10-27 21:14 UTC");
        assert_eq!(b.semver().to_string(), "0.1.0+051465f");
        assert_eq!(
            b.one_line(),
            "Hadion 0.1.0 (051465f, dirty) x86_64-pc-windows-gnu release"
        );
    }

    #[test]
    fn github_remotes() {
        for (url, want) in [
            (
                "https://github.com/saziknows/hadion-gui.git",
                Some("saziknows/hadion-gui"),
            ),
            ("git@github.com:o/r.git", Some("o/r")),
            ("ssh://git@github.com/o/r", Some("o/r")),
            ("https://user:ghp_secret@github.com/o/r.git", Some("o/r")),
            ("https://github.com/o/r/", Some("o/r")),
            ("https://gitlab.com/o/r.git", None),
            ("https://github.com/o", None),
            ("https://github.com/o/r/tree/main", None),
            ("C:/repos/x", None),
        ] {
            assert_eq!(github_repo_of_url(url).as_deref(), want, "{url}");
        }
    }
}
