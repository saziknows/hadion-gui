//! Container images on the GitHub Container Registry (GHCR).
//!
//! Packages belong to a user or an organisation, not to a repository, so
//! listing them needs the owner's type (`GET repos/o/r` → `owner.type`) and
//! then `users/{owner}/packages` or `orgs/{owner}/packages`. A package
//! records the repository it is linked to (`repository.full_name`); the
//! view keeps those and offers all of the owner's packages.
//!
//! Every packages endpoint needs the `read:packages` scope, which a plain
//! `gh auth login` does not grant: [`problem`] recognises that answer so the
//! view can show the exact fix ([`SCOPE_FIX`]). gitvis never runs it, since
//! it opens a browser and changes the user's authentication.

use serde::Deserialize;

use crate::Result;
use crate::devops::gh::{Gh, Request, encode};
use crate::github::{ApiCall, parse_time};

/// The command that grants the missing scope (the user runs it).
pub const SCOPE_FIX: &str = "gh auth refresh -h github.com -s read:packages";
/// Deleting versions needs one more scope.
pub const DELETE_SCOPE_FIX: &str = "gh auth refresh -h github.com -s read:packages,delete:packages";
/// Versions fetched per package.
pub const VERSIONS_PER_PAGE: usize = 50;

/// Who owns the packages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerKind {
    User,
    Org,
}

impl OwnerKind {
    /// From `owner.type` (`User`, `Organization`).
    pub fn from_type(t: &str) -> Self {
        if t.eq_ignore_ascii_case("organization") {
            OwnerKind::Org
        } else {
            OwnerKind::User
        }
    }

    /// The URL segment: `users` or `orgs`.
    pub fn segment(self) -> &'static str {
        match self {
            OwnerKind::User => "users",
            OwnerKind::Org => "orgs",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            OwnerKind::User => "user",
            OwnerKind::Org => "organisation",
        }
    }
}

/// `GET repos/{o}/{r}`: only what is needed here.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct RepoInfo {
    pub full_name: String,
    pub owner: Owner,
    pub private: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Owner {
    pub login: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct RepoRef {
    pub full_name: String,
}

/// One package (`GET users/{o}/packages?package_type=container`).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Package {
    pub id: u64,
    pub name: String,
    pub package_type: String,
    pub visibility: String,
    pub version_count: u64,
    pub html_url: String,
    pub url: String,
    pub created_at: String,
    pub updated_at: String,
    pub repository: Option<RepoRef>,
    pub owner: Option<Owner>,
}

impl Package {
    /// Linked to `full_name` (`owner/repo`, case-insensitive)?
    pub fn linked_to(&self, full_name: &str) -> bool {
        self.repository
            .as_ref()
            .is_some_and(|r| r.full_name.eq_ignore_ascii_case(full_name))
    }

    pub fn repository_name(&self) -> Option<&str> {
        self.repository
            .as_ref()
            .map(|r| r.full_name.as_str())
            .filter(|s| !s.is_empty())
    }

    pub fn updated(&self) -> Option<i64> {
        parse_time(&self.updated_at)
    }

    pub fn created(&self) -> Option<i64> {
        parse_time(&self.created_at)
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ContainerMeta {
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct VersionMeta {
    pub package_type: String,
    pub container: Option<ContainerMeta>,
}

/// One version of a container package: an image digest with its tags.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct PackageVersion {
    pub id: u64,
    /// The manifest digest, `sha256:…`.
    pub name: String,
    pub html_url: String,
    pub package_html_url: String,
    pub created_at: String,
    pub updated_at: String,
    pub metadata: Option<VersionMeta>,
}

impl PackageVersion {
    pub fn tags(&self) -> &[String] {
        self.metadata
            .as_ref()
            .and_then(|m| m.container.as_ref())
            .map(|c| c.tags.as_slice())
            .unwrap_or(&[])
    }

    pub fn digest(&self) -> &str {
        &self.name
    }

    /// `sha256:1a2b3c4d5e6f` (12 hex digits), for lists.
    pub fn short_digest(&self) -> String {
        short_digest(&self.name)
    }

    pub fn created(&self) -> Option<i64> {
        parse_time(&self.created_at)
    }

    pub fn updated(&self) -> Option<i64> {
        parse_time(&self.updated_at)
    }
}

/// `sha256:1a2b3c4d5e6f…` → `sha256:1a2b3c4d5e6f`.
pub fn short_digest(d: &str) -> String {
    match d.split_once(':') {
        Some((algo, hex)) => format!("{algo}:{}", &hex[..hex.len().min(12)]),
        None => d.chars().take(19).collect(),
    }
}

/// `{users|orgs}/{owner}/packages?package_type=container`.
pub fn list_endpoint(kind: OwnerKind, owner: &str) -> String {
    format!(
        "{}/{}/packages?package_type=container",
        kind.segment(),
        encode(owner)
    )
}

/// `…/packages/container/{name}/versions?per_page=50`. Package names may
/// contain `/` (`shop/api-gateway`), which is encoded.
pub fn versions_endpoint(kind: OwnerKind, owner: &str, name: &str) -> String {
    format!(
        "{}/{}/packages/container/{}/versions?per_page={VERSIONS_PER_PAGE}",
        kind.segment(),
        encode(owner),
        encode(name)
    )
}

/// `DELETE …/packages/container/{name}/versions/{id}` (needs
/// `delete:packages` and admin rights on the package).
pub fn delete_version(kind: OwnerKind, owner: &str, name: &str, id: u64) -> Request {
    Request::delete(format!(
        "{}/{}/packages/container/{}/versions/{id}",
        kind.segment(),
        encode(owner),
        encode(name)
    ))
}

/// `ghcr.io/{owner}/{name}` (the registry wants lower case).
pub fn image(owner: &str, name: &str) -> String {
    format!(
        "ghcr.io/{}/{}",
        owner.to_ascii_lowercase(),
        name.to_ascii_lowercase()
    )
}

/// `docker pull ghcr.io/owner/name:tag`.
pub fn pull_command(owner: &str, name: &str, tag: &str) -> String {
    format!("docker pull {}:{tag}", image(owner, name))
}

/// `docker pull ghcr.io/owner/name@sha256:…`.
pub fn pull_by_digest(owner: &str, name: &str, digest: &str) -> String {
    format!("docker pull {}@{digest}", image(owner, name))
}

/// Why a packages call failed, as far as the answer tells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    /// GitHub said so: the token lacks `read:packages`.
    MissingScope,
    /// HTTP 403 / 404 without naming the scope: almost always the same
    /// cause (GitHub hides what a token may not see).
    LikelyScope,
    /// Deleting needs `delete:packages`.
    MissingDeleteScope,
    Other,
}

impl Problem {
    pub fn is_scope(self) -> bool {
        !matches!(self, Problem::Other)
    }

    /// The command that fixes it, when there is one.
    pub fn fix(self) -> Option<&'static str> {
        match self {
            Problem::MissingScope | Problem::LikelyScope => Some(SCOPE_FIX),
            Problem::MissingDeleteScope => Some(DELETE_SCOPE_FIX),
            Problem::Other => None,
        }
    }
}

/// Classify a packages error message.
pub fn problem(err: &str) -> Problem {
    let l = err.to_ascii_lowercase();
    if l.contains("delete:packages") {
        Problem::MissingDeleteScope
    } else if l.contains("read:packages") || l.contains("write:packages") {
        Problem::MissingScope
    } else if l.contains("http 403") || l.contains("http 404") {
        Problem::LikelyScope
    } else {
        Problem::Other
    }
}

/// What the Packages tab loads when it opens.
#[derive(Clone, Debug)]
pub struct Listing {
    pub owner: String,
    pub kind: OwnerKind,
    pub repo: String,
    pub private: bool,
    /// Every container package of the owner (the view filters).
    pub packages: Vec<Package>,
}

/// The owner's type, then all of its container packages (up to 5 pages of
/// 100). The owner and its type are returned even when the listing fails,
/// so the error can say whose packages were asked for.
pub fn load(gh: &Gh, calls: &mut Vec<ApiCall>) -> (Option<(String, OwnerKind)>, Result<Listing>) {
    let info: RepoInfo = match gh.get_json(&gh.path(""), "repository", calls) {
        Ok(i) => i,
        Err(e) => return (None, Err(e)),
    };
    let owner = if info.owner.login.is_empty() {
        gh.owner.clone()
    } else {
        info.owner.login.clone()
    };
    let kind = OwnerKind::from_type(&info.owner.kind);
    let who = Some((owner.clone(), kind));
    let packages: Vec<Package> =
        match gh.get_pages(&list_endpoint(kind, &owner), "container packages", 5, calls) {
            Ok(p) => p,
            Err(e) => return (who, Err(e)),
        };
    let repo = if info.full_name.is_empty() {
        gh.full_name()
    } else {
        info.full_name
    };
    (
        who,
        Ok(Listing {
            owner,
            kind,
            repo,
            private: info.private,
            packages,
        }),
    )
}

/// The newest versions of one package.
pub fn load_versions(
    gh: &Gh,
    kind: OwnerKind,
    owner: &str,
    name: &str,
    calls: &mut Vec<ApiCall>,
) -> Result<Vec<PackageVersion>> {
    gh.get_json(
        &versions_endpoint(kind, owner, name),
        "package versions",
        calls,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devops::gh::Method;

    const PACKAGES: &str = r#"[
      {"id": 4183913, "name": "shop/api-gateway", "package_type": "container",
       "owner": {"login": "acme", "id": 1, "type": "User"},
       "version_count": 37, "visibility": "private",
       "url": "https://api.github.com/users/acme/packages/container/shop%2Fapi-gateway",
       "created_at": "2026-09-18T21:44:02Z", "updated_at": "2026-10-25T08:12:44Z",
       "repository": {"id": 9, "name": "shop", "full_name": "acme/shop", "private": true},
       "html_url": "https://github.com/users/acme/packages/container/package/shop%2Fapi-gateway"},
      {"id": 4183999, "name": "dotfiles-runner", "package_type": "container",
       "owner": {"login": "acme", "type": "User"}, "version_count": 2,
       "visibility": "public", "created_at": "2025-01-02T10:00:00Z",
       "updated_at": "2025-01-03T10:00:00Z"}
    ]"#;

    const VERSIONS: &str = r#"[
      {"id": 301562871, "name": "sha256:9f2c1d0b7a4e5f60718293a4b5c6d7e8f9011223344556677889900aabbccdd",
       "url": "https://api.github.com/users/acme/packages/container/shop%2Fapi-gateway/versions/301562871",
       "package_html_url": "https://github.com/users/acme/packages/container/package/shop%2Fapi-gateway",
       "created_at": "2026-10-25T08:12:40Z", "updated_at": "2026-10-25T08:12:44Z",
       "html_url": "https://github.com/users/acme/packages/container/shop%2Fapi-gateway/301562871",
       "metadata": {"package_type": "container", "container": {"tags": ["latest", "main", "sha-1a2b3c4"]}}},
      {"id": 301000001, "name": "sha256:00aa", "created_at": "2026-10-20T08:00:00Z",
       "updated_at": "2026-10-20T08:00:00Z",
       "metadata": {"package_type": "container", "container": {"tags": []}}}
    ]"#;

    #[test]
    fn endpoints_encode_names() {
        assert_eq!(
            list_endpoint(OwnerKind::User, "acme"),
            "users/acme/packages?package_type=container"
        );
        assert_eq!(
            list_endpoint(OwnerKind::Org, "brave"),
            "orgs/brave/packages?package_type=container"
        );
        assert_eq!(
            versions_endpoint(OwnerKind::User, "acme", "shop/api-gateway"),
            "users/acme/packages/container/shop%2Fapi-gateway/versions?per_page=50"
        );
    }

    #[test]
    fn delete_request() {
        let r = delete_version(OwnerKind::Org, "brave", "brave-core/ci", 77);
        assert_eq!(r.method, Method::Delete);
        assert_eq!(
            r.endpoint,
            "orgs/brave/packages/container/brave-core%2Fci/versions/77"
        );
        assert_eq!(r.body, None);
        assert_eq!(
            r.display(),
            "gh api --method DELETE orgs/brave/packages/container/brave-core%2Fci/versions/77"
        );
        let u = delete_version(OwnerKind::User, "acme", "web", 5);
        assert_eq!(u.endpoint, "users/acme/packages/container/web/versions/5");
    }

    #[test]
    fn packages_parse_and_link() {
        let p: Vec<Package> = serde_json::from_str(PACKAGES).unwrap();
        assert_eq!(p.len(), 2);
        assert!(p[0].linked_to("Acme/Shop"));
        assert!(!p[1].linked_to("acme/shop"));
        assert_eq!(p[1].repository_name(), None);
        assert_eq!(p[0].version_count, 37);
        assert_eq!(p[0].updated(), parse_time("2026-10-25T08:12:44Z"));
        assert_eq!(OwnerKind::from_type("Organization"), OwnerKind::Org);
        assert_eq!(OwnerKind::from_type("User"), OwnerKind::User);
    }

    #[test]
    fn versions_parse_with_tags_and_digests() {
        let v: Vec<PackageVersion> = serde_json::from_str(VERSIONS).unwrap();
        assert_eq!(v[0].tags(), ["latest", "main", "sha-1a2b3c4"]);
        assert_eq!(v[0].short_digest(), "sha256:9f2c1d0b7a4e");
        assert!(v[1].tags().is_empty());
        assert_eq!(v[1].short_digest(), "sha256:00aa");
        assert_eq!(
            pull_command("Acme", "shop/api-gateway", "main"),
            "docker pull ghcr.io/acme/shop/api-gateway:main"
        );
        assert_eq!(
            pull_by_digest("o", "n", "sha256:00aa"),
            "docker pull ghcr.io/o/n@sha256:00aa"
        );
    }

    #[test]
    fn scope_errors_are_recognised() {
        let e = "You need at least read:packages scope to list packages. (HTTP 403)\nSee https://docs.github.com/rest/packages/packages#list-packages-for-a-user";
        assert_eq!(problem(e), Problem::MissingScope);
        assert_eq!(problem(e).fix(), Some(SCOPE_FIX));
        assert_eq!(problem("Not Found (HTTP 404)"), Problem::LikelyScope);
        assert_eq!(
            problem("You need delete:packages scope (HTTP 403)"),
            Problem::MissingDeleteScope
        );
        assert_eq!(problem("timeout"), Problem::Other);
        assert!(!Problem::Other.is_scope());
        assert_eq!(problem("x").fix(), None);
    }
}
