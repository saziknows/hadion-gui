//! Which commit a container is running.
//!
//! Best evidence first: the image's revision label
//! (`org.opencontainers.image.revision`, also `org.label-schema.vcs-ref`),
//! then its tags (`sha-1a2b3c4`, `v1.2.3`, branch names, `pr-12`) and the
//! reference the container was created from, then the version label (only
//! when the image's source is this repository: base images set it to their
//! own version). Registry digests are matched by the app through the GHCR
//! packages it has loaded. Images built by compose usually carry none of
//! this, which [`Revision::compose_built`] lets the view say plainly.

use super::model::{Container, Image};
use crate::Oid;
use crate::devops::runtime::kube::parse_image;
use crate::devops::runtime::matching::{MatchKind, RefIndex, RefMatch};

/// Labels that name the commit an image was built from.
pub const REVISION_LABELS: [&str; 2] = [
    "org.opencontainers.image.revision",
    "org.label-schema.vcs-ref",
];
/// Labels that name the repository an image was built from.
pub const SOURCE_LABELS: [&str; 2] = [
    "org.opencontainers.image.source",
    "org.label-schema.vcs-url",
];
pub const VERSION_LABEL: &str = "org.opencontainers.image.version";

/// Where an answer came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Evidence {
    /// A revision label: `(key, value)`.
    Label(String, String),
    /// An image tag.
    Tag(String),
    /// `org.opencontainers.image.version`.
    Version(String),
    /// A registry digest whose package tags name it.
    Digest(String),
}

impl Evidence {
    /// `revision label`, `image tag sha-1a2b3c4`.
    pub fn text(&self) -> String {
        match self {
            Evidence::Label(k, _) => format!("label {k}"),
            Evidence::Tag(t) => format!("image tag {t}"),
            Evidence::Version(v) => format!("version label {v}"),
            Evidence::Digest(d) => format!("digest {}", short_digest(d)),
        }
    }
}

/// `sha256:0123456789ab…` → `sha256:0123456789ab`.
pub fn short_digest(d: &str) -> String {
    let (algo, hex) = d.split_once(':').unwrap_or(("", d));
    // Characters, not bytes: labels and tags are free text.
    let hex: String = hex.chars().take(12).collect();
    if algo.is_empty() {
        hex.to_owned()
    } else {
        format!("{algo}:{hex}")
    }
}

/// What the image says about its origin, and the match found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Revision {
    /// The revision label, also when it names nothing in this repository
    /// (inherited from a base image, or a commit not fetched).
    pub label: Option<(String, String)>,
    /// `org.opencontainers.image.source`.
    pub source: Option<String>,
    /// The source is one of the repository's remotes.
    pub source_matches: bool,
    /// The best match, with what it came from.
    pub found: Option<(Evidence, RefMatch)>,
    /// The image carries compose's labels (compose built it).
    pub compose_built: bool,
    /// The image's tags (the part after `:`), and the container's.
    pub tags: Vec<String>,
}

impl Revision {
    pub fn commit(&self) -> Option<Oid> {
        self.found.as_ref().and_then(|(_, m)| m.commit)
    }
}

fn label<'a>(
    c: &'a Container,
    image: Option<&'a Image>,
    keys: &[&str],
) -> Option<(String, String)> {
    for k in keys {
        let v = c
            .label(k)
            .filter(|v| !v.trim().is_empty())
            .or_else(|| image.and_then(|i| i.label(k)));
        if let Some(v) = v {
            return Some(((*k).to_owned(), v.trim().to_owned()));
        }
    }
    None
}

/// The tags of an image and of the reference the container uses.
pub fn tags_of(c: &Container, image: Option<&Image>) -> Vec<String> {
    let mut tags: Vec<String> = image
        .map(|i| i.tags.iter().filter_map(|t| parse_image(t).tag).collect())
        .unwrap_or_default();
    if let Some(t) = parse_image(&c.image).tag {
        tags.push(t);
    }
    tags.retain(|t| !t.is_empty());
    tags.sort();
    tags.dedup();
    tags
}

/// What the container's image says, matched against the repository.
pub fn revision(
    idx: Option<&RefIndex>,
    c: &Container,
    image: Option<&Image>,
    remotes: &[String],
) -> Revision {
    let lbl = label(c, image, &REVISION_LABELS);
    let source = label(c, image, &SOURCE_LABELS).map(|(_, v)| v);
    let source_matches = source
        .as_deref()
        .is_some_and(|s| source_matches(s, remotes));
    let compose_built = image.is_some_and(|i| i.label(super::model::LABEL_PROJECT).is_some());
    let tags = tags_of(c, image);
    let mut r = Revision {
        label: lbl.clone(),
        source,
        source_matches,
        found: None,
        compose_built,
        tags: tags.clone(),
    };
    let Some(idx) = idx else {
        return r;
    };
    if let Some((k, v)) = &lbl
        && let Some(m) = idx.match_revision(v)
        && m.commit.is_some()
    {
        r.found = Some((Evidence::Label(k.clone(), v.clone()), m));
        return r;
    }
    let by_tag = tags
        .iter()
        .filter_map(|t| idx.match_tag(t).map(|m| (t.clone(), m)))
        .min_by_key(|(_, m)| (m.commit.is_none(), m.kind));
    if let Some((t, m)) = &by_tag
        && m.commit.is_some()
    {
        r.found = Some((Evidence::Tag(t.clone()), m.clone()));
        return r;
    }
    if source_matches
        && let Some(v) = label(c, image, &[VERSION_LABEL]).map(|(_, v)| v)
        && let Some(m) = idx.match_tag(&v)
        && m.commit.is_some()
    {
        r.found = Some((Evidence::Version(v), m));
        return r;
    }
    // A compose build's `latest` says nothing: the compose note says more.
    r.found = by_tag
        .filter(|(_, m)| !(compose_built && m.kind == MatchKind::Floating))
        .map(|(t, m)| (Evidence::Tag(t), m));
    r
}

/// `(host, path)` of a git remote or image source URL: `https://`,
/// `ssh://`, `git://`, `git@host:owner/repo`; credentials and ports
/// dropped, `.git` trimmed. Local paths give `None`.
pub fn repo_parts(url: &str) -> Option<(String, String)> {
    let u = url.trim().trim_end_matches('/');
    let u = u.strip_suffix(".git").unwrap_or(u);
    let (host, path) = if let Some((scheme, rest)) = u.split_once("://") {
        if !matches!(
            scheme.to_ascii_lowercase().as_str(),
            "http" | "https" | "ssh" | "git" | "git+ssh"
        ) {
            return None;
        }
        let (auth, path) = rest.split_once('/')?;
        let host = auth.rsplit_once('@').map_or(auth, |(_, h)| h);
        (host.split(':').next()?.to_owned(), path.to_owned())
    } else {
        // scp-like `[user@]host:path`; `C:\…` and `./x` are local.
        let (left, path) = u.split_once(':')?;
        if left.len() < 2 || left.contains(['/', '\\']) || path.starts_with('\\') {
            return None;
        }
        let host = left.rsplit_once('@').map_or(left, |(_, h)| h);
        (host.to_owned(), path.to_owned())
    };
    let path = path.trim_matches('/').to_owned();
    if host.is_empty() || path.is_empty() || !host.contains('.') && host != "localhost" {
        return None;
    }
    Some((host.to_ascii_lowercase(), path))
}

/// `github.com/acme/shop` (lower case), for comparing remotes and sources.
pub fn repo_key(url: &str) -> Option<String> {
    repo_parts(url).map(|(h, p)| format!("{h}/{}", p.to_ascii_lowercase()))
}

/// The https URL of a remote without credentials (for
/// `org.opencontainers.image.source`): `git@github.com:acme/shop.git` →
/// `https://github.com/acme/shop`.
pub fn https_url(remote: &str) -> Option<String> {
    repo_parts(remote).map(|(h, p)| format!("https://{h}/{p}"))
}

/// Does an image source name one of these remotes?
pub fn source_matches(source: &str, remotes: &[String]) -> bool {
    let Some(s) = repo_key(source) else {
        return false;
    };
    remotes
        .iter()
        .any(|r| repo_key(r).as_deref() == Some(s.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(n: u8) -> Oid {
        let mut b = [0u8; 20];
        b[0] = n;
        b[19] = n;
        Oid::from_bytes(&b).unwrap()
    }

    fn idx() -> RefIndex {
        RefIndex::new(
            [oid(0xab), oid(0xcd)],
            vec![("v1.2.3".into(), oid(0xcd))],
            vec![("main".into(), oid(0xab))],
            vec![],
            Some(oid(0xab)),
            None,
        )
    }

    fn container(image: &str, labels: &[(&str, &str)]) -> Container {
        Container {
            image: image.into(),
            labels: labels
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            ..Container::default()
        }
    }

    #[test]
    fn the_revision_label_wins() {
        let ab = oid(0xab).to_string();
        let c = container(
            "ghcr.io/acme/shop:v1.2.3",
            &[("org.opencontainers.image.revision", ab.as_str())],
        );
        let r = revision(Some(&idx()), &c, None, &[]);
        let (ev, m) = r.found.clone().unwrap();
        assert!(matches!(ev, Evidence::Label(..)));
        assert_eq!(m.commit, Some(oid(0xab)));
        assert_eq!(r.commit(), Some(oid(0xab)));
    }

    #[test]
    fn tags_then_version_label() {
        let c = container("shop:v1.2.3", &[]);
        let r = revision(Some(&idx()), &c, None, &[]);
        assert_eq!(r.found.as_ref().unwrap().1.kind, MatchKind::Tag);
        assert_eq!(r.commit(), Some(oid(0xcd)));
        // A revision label that names nothing here is kept, the tag used.
        let c = container(
            "shop:v1.2.3",
            &[(
                "org.label-schema.vcs-ref",
                "0000000000000000000000000000000000000001",
            )],
        );
        let r = revision(Some(&idx()), &c, None, &[]);
        assert_eq!(r.label.as_ref().unwrap().0, "org.label-schema.vcs-ref");
        assert!(matches!(r.found.as_ref().unwrap().0, Evidence::Tag(_)));
        // The version label counts only for this repository's images.
        let img = Image {
            tags: vec!["shop:latest".into()],
            labels: [
                (
                    "org.opencontainers.image.version".to_owned(),
                    "1.2.3".to_owned(),
                ),
                (
                    "org.opencontainers.image.source".to_owned(),
                    "https://github.com/acme/shop".to_owned(),
                ),
            ]
            .into_iter()
            .collect(),
            ..Image::default()
        };
        let c = container("shop:latest", &[]);
        let mine = vec!["git@github.com:acme/shop.git".to_owned()];
        let r = revision(Some(&idx()), &c, Some(&img), &mine);
        assert!(r.source_matches);
        assert!(matches!(r.found.as_ref().unwrap().0, Evidence::Version(_)));
        let r = revision(Some(&idx()), &c, Some(&img), &[]);
        assert_eq!(r.found.as_ref().unwrap().1.kind, MatchKind::Floating);
        assert_eq!(r.commit(), None);
    }

    #[test]
    fn compose_built_images_say_so() {
        let img = Image {
            labels: [("com.docker.compose.project".to_owned(), "shop".to_owned())]
                .into_iter()
                .collect(),
            ..Image::default()
        };
        let r = revision(Some(&idx()), &container("shop-web", &[]), Some(&img), &[]);
        assert!(r.compose_built);
        assert!(r.found.is_none());
        // Its `latest` tag is not worth a match either.
        let r = revision(
            Some(&idx()),
            &container("shop-web:latest", &[]),
            Some(&img),
            &[],
        );
        assert!(r.found.is_none());
    }

    #[test]
    fn remote_urls() {
        for (url, key) in [
            (
                "https://github.com/Acme/Shop.git",
                Some("github.com/acme/shop"),
            ),
            ("git@github.com:acme/shop.git", Some("github.com/acme/shop")),
            (
                "ssh://git@github.com:22/acme/shop",
                Some("github.com/acme/shop"),
            ),
            (
                "https://user:token@gitlab.example.com/g/sub/proj",
                Some("gitlab.example.com/g/sub/proj"),
            ),
            ("C:\\repos\\shop", None),
            ("../shop", None),
            ("/srv/git/shop.git", None),
        ] {
            assert_eq!(repo_key(url).as_deref(), key, "{url}");
        }
        assert_eq!(
            https_url("https://user:token@github.com/Acme/Shop.git").as_deref(),
            Some("https://github.com/Acme/Shop")
        );
        assert_eq!(
            https_url("git@github.com:acme/shop.git").as_deref(),
            Some("https://github.com/acme/shop")
        );
        assert!(source_matches(
            "https://github.com/acme/shop",
            &["git@github.com:ACME/shop.git".into()]
        ));
        assert!(!source_matches(
            "https://github.com/acme/other",
            &["git@github.com:acme/shop.git".into()]
        ));
        assert_eq!(
            short_digest("sha256:0123456789abcdef"),
            "sha256:0123456789ab"
        );
    }
}
