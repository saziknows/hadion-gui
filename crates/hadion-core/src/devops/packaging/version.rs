//! Semantic versions with semver 2.0 precedence (pre-releases sort before
//! their release, numeric identifiers numerically, build metadata ignored)
//! and a lenient parser for release tags (`v1.2`, `V1.2.3-rc.1+abc`).

use std::cmp::Ordering;
use std::fmt;

/// One dot-separated pre-release identifier.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ident {
    Num(u64),
    Alpha(String),
}

impl Ident {
    fn parse(s: &str) -> Option<Ident> {
        if s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return None;
        }
        if s.chars().all(|c| c.is_ascii_digit()) {
            s.parse().ok().map(Ident::Num)
        } else {
            Some(Ident::Alpha(s.to_owned()))
        }
    }
}

impl Ord for Ident {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Ident::Num(a), Ident::Num(b)) => a.cmp(b),
            // Numeric identifiers have lower precedence than alphanumeric ones.
            (Ident::Num(_), Ident::Alpha(_)) => Ordering::Less,
            (Ident::Alpha(_), Ident::Num(_)) => Ordering::Greater,
            (Ident::Alpha(a), Ident::Alpha(b)) => a.cmp(b),
        }
    }
}

impl PartialOrd for Ident {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Ident {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ident::Num(n) => write!(f, "{n}"),
            Ident::Alpha(s) => f.write_str(s),
        }
    }
}

/// `MAJOR.MINOR.PATCH[-PRE][+BUILD]`. Equality and ordering follow semver
/// precedence, so `1.0.0+a == 1.0.0+b`.
#[derive(Clone, Debug)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Vec<Ident>,
    /// Build metadata (after `+`), kept for display only.
    pub build: String,
}

impl Version {
    pub fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
            pre: Vec::new(),
            build: String::new(),
        }
    }

    /// Parse a version or a release tag. Accepts a leading `v`, missing
    /// minor / patch numbers (`v2` = `2.0.0`) and surrounding spaces.
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        let s = s
            .strip_prefix('v')
            .or_else(|| s.strip_prefix('V'))
            .unwrap_or(s);
        let (rest, build) = match s.split_once('+') {
            Some((r, b)) => {
                if b.is_empty()
                    || !b.split('.').all(|p| {
                        !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                    })
                {
                    return None;
                }
                (r, b.to_owned())
            }
            None => (s, String::new()),
        };
        let (core, pre) = match rest.split_once('-') {
            Some((c, p)) => {
                let ids: Option<Vec<Ident>> = p.split('.').map(Ident::parse).collect();
                (c, ids?)
            }
            None => (rest, Vec::new()),
        };
        let mut nums = core.split('.');
        let mut next = |required: bool| -> Option<u64> {
            match nums.next() {
                Some(n) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => n.parse().ok(),
                Some(_) => None,
                None if required => None,
                None => Some(0),
            }
        };
        let major = next(true)?;
        let minor = next(false)?;
        let patch = next(false)?;
        if nums.next().is_some() {
            return None;
        }
        Some(Version {
            major,
            minor,
            patch,
            pre,
            build,
        })
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }

    /// `1.2.3-rc.1` (no build metadata): the form used in asset names.
    pub fn plain(&self) -> String {
        let mut s = format!("{}.{}.{}", self.major, self.minor, self.patch);
        if !self.pre.is_empty() {
            s.push('-');
            s.push_str(
                &self
                    .pre
                    .iter()
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join("."),
            );
        }
        s
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then(self.minor.cmp(&other.minor))
            .then(self.patch.cmp(&other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                // A release outranks its pre-releases.
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => self.pre.cmp(&other.pre),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.plain())?;
        if !self.build.is_empty() {
            write!(f, "+{}", self.build)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("{s} should parse"))
    }

    #[test]
    fn parses_tags_leniently() {
        assert_eq!(v("v1.2.3"), Version::new(1, 2, 3));
        assert_eq!(v("V2"), Version::new(2, 0, 0));
        assert_eq!(v(" 0.4 "), Version::new(0, 4, 0));
        let x = v("1.0.0-rc.1+build.7");
        assert_eq!(x.pre, vec![Ident::Alpha("rc".into()), Ident::Num(1)]);
        assert_eq!(x.build, "build.7");
        assert_eq!(x.to_string(), "1.0.0-rc.1+build.7");
        assert_eq!(x.plain(), "1.0.0-rc.1");
        for bad in [
            "",
            "v",
            "1.2.3.4",
            "1..2",
            "1.x",
            "nightly",
            "1.0.0-",
            "1.0.0+",
            "1.0.0-a..b",
            "1.0.0-ä",
        ] {
            assert!(Version::parse(bad).is_none(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn semver_precedence_example_from_the_spec() {
        // https://semver.org/#spec-item-11
        let order = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
            "1.0.1",
            "1.1.0",
            "2.0.0",
            "10.0.0",
        ];
        for w in order.windows(2) {
            assert!(v(w[0]) < v(w[1]), "{} < {}", w[0], w[1]);
        }
    }

    #[test]
    fn build_metadata_is_ignored_for_precedence() {
        assert_eq!(v("1.0.0+aaa"), v("1.0.0+bbb"));
        assert!(v("0.2.0") > v("0.1.0+051465f"));
        assert!(v("0.1.0-rc.2") < v("0.1.0"));
        assert!(v("0.10.0") > v("0.9.9"));
        assert!(!v("1.0.0").is_prerelease() && v("1.0.0-x").is_prerelease());
    }
}
