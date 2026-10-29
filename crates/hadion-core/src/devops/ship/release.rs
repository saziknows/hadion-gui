//! GitHub releases: the JSON model, the reads, and every write as a pure
//! builder (REST [`Request`]s for edit / delete / generate-notes, `gh`
//! argument lists for create and download).

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::devops::gh::{Gh, Request, encode};
use crate::github::{ApiCall, parse_time};
use crate::{Error, Result};

/// Releases per page of the list.
pub const PAGE: u32 = 30;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct User {
    pub login: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Asset {
    pub id: u64,
    pub name: String,
    pub label: Option<String>,
    pub content_type: String,
    /// `uploaded` or `open` (an upload in progress).
    pub state: String,
    pub size: u64,
    pub download_count: u64,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub browser_download_url: String,
    pub uploader: Option<User>,
    pub digest: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Release {
    pub id: u64,
    pub tag_name: String,
    pub name: Option<String>,
    pub body: Option<String>,
    pub draft: bool,
    pub prerelease: bool,
    pub immutable: bool,
    /// Branch or commit the tag was (or will be) created from.
    pub target_commitish: String,
    pub created_at: Option<String>,
    pub published_at: Option<String>,
    pub html_url: String,
    pub author: Option<User>,
    pub assets: Vec<Asset>,
}

impl Release {
    /// The name, or the tag when the release has none.
    pub fn title(&self) -> &str {
        match self.name.as_deref().map(str::trim) {
            Some(n) if !n.is_empty() => n,
            _ => &self.tag_name,
        }
    }

    pub fn published(&self) -> Option<i64> {
        self.published_at.as_deref().and_then(parse_time)
    }

    pub fn created(&self) -> Option<i64> {
        self.created_at.as_deref().and_then(parse_time)
    }

    /// Published time, else created (drafts).
    pub fn time(&self) -> Option<i64> {
        self.published().or_else(|| self.created())
    }

    pub fn downloads(&self) -> u64 {
        self.assets.iter().map(|a| a.download_count).sum()
    }

    pub fn asset_bytes(&self) -> u64 {
        self.assets.iter().map(|a| a.size).sum()
    }

    pub fn author_login(&self) -> &str {
        self.author.as_ref().map(|a| a.login.as_str()).unwrap_or("")
    }
}

/// `repos/o/r/releases?per_page=30&page=N`.
pub fn list_endpoint(gh: &Gh, page: u32) -> String {
    gh.path(&format!("releases?per_page={PAGE}&page={}", page.max(1)))
}

/// One page of releases, newest first.
pub fn fetch_page(gh: &Gh, page: u32, calls: &mut Vec<ApiCall>) -> Result<Vec<Release>> {
    gh.get_json(&list_endpoint(gh, page), "releases", calls)
}

/// The id of the release marked latest (`None` when there is none: GitHub
/// answers 404).
pub fn fetch_latest_id(gh: &Gh, calls: &mut Vec<ApiCall>) -> Option<u64> {
    let v: Value = gh
        .get_json(&gh.path("releases/latest"), "latest release", calls)
        .ok()?;
    v.get("id").and_then(Value::as_u64)
}

/// Up to `max_pages` pages of 100 (for metrics).
pub fn fetch_many(gh: &Gh, max_pages: usize, calls: &mut Vec<ApiCall>) -> Result<Vec<Release>> {
    gh.get_pages(&gh.path("releases"), "releases", max_pages, calls)
}

/// POST `releases/generate-notes`: GitHub writes the notes it would use
/// for a release of `tag` (changes nothing).
pub fn generate_notes_request(
    gh: &Gh,
    tag: &str,
    target: &str,
    previous_tag: Option<&str>,
) -> Request {
    let mut body = json!({ "tag_name": tag });
    if !target.trim().is_empty() {
        body["target_commitish"] = json!(target.trim());
    }
    if let Some(p) = previous_tag.map(str::trim).filter(|p| !p.is_empty()) {
        body["previous_tag_name"] = json!(p);
    }
    Request::post(gh.path("releases/generate-notes"), body)
}

/// The generated notes: `(name, body)`.
pub fn generate_notes(
    gh: &Gh,
    req: &Request,
    calls: &mut Vec<ApiCall>,
) -> Result<(String, String)> {
    let v: Value = gh.send_json(req, "generated notes", calls).map_err(|e| {
        let m = e.to_string();
        if m.contains("(HTTP 404)") {
            Error::new(format!(
                "{m}
GitHub answers 404 here when your account cannot push to the repository: generating notes needs write access. Generate them locally instead."
            ))
        } else {
            e
        }
    })?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    Ok((s("name"), s("body")))
}

/// What an edit changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReleaseEdit {
    pub name: String,
    pub body: String,
    pub draft: bool,
    pub prerelease: bool,
    /// `Some(true)` marks the release latest, `Some(false)` not; `None`
    /// leaves it to GitHub.
    pub make_latest: Option<bool>,
}

/// PATCH `releases/{id}`.
pub fn edit_request(gh: &Gh, id: u64, e: &ReleaseEdit) -> Request {
    let mut body = json!({
        "name": e.name,
        "body": e.body,
        "draft": e.draft,
        "prerelease": e.prerelease,
    });
    if let Some(l) = e.make_latest {
        // GitHub takes the strings "true" / "false" / "legacy" here.
        body["make_latest"] = json!(if l { "true" } else { "false" });
    }
    Request::patch(gh.path(&format!("releases/{id}")), body)
}

/// DELETE `releases/{id}` (the tag stays).
pub fn delete_request(gh: &Gh, id: u64) -> Request {
    Request::delete(gh.path(&format!("releases/{id}")))
}

/// DELETE `git/refs/tags/{tag}`: removes the tag on GitHub.
pub fn delete_tag_request(gh: &Gh, tag: &str) -> Request {
    let path = tag.split('/').map(encode).collect::<Vec<_>>().join("/");
    Request::delete(gh.path(&format!("git/refs/tags/{path}")))
}

/// A release to create with `gh release create`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NewRelease {
    pub tag: String,
    /// Branch or commit to create the tag from (ignored by GitHub when the
    /// tag exists).
    pub target: String,
    pub title: String,
    pub notes: String,
    pub draft: bool,
    pub prerelease: bool,
    /// `false` passes `--latest=false`; `true` lets GitHub decide (the
    /// newest non-prerelease by date and version becomes latest).
    pub latest: bool,
    pub files: Vec<String>,
}

impl NewRelease {
    /// Why it cannot be published yet, if anything.
    pub fn problem(&self) -> Option<&'static str> {
        let tag = self.tag.trim();
        if tag.is_empty() {
            return Some("Choose or type a tag.");
        }
        if tag.contains(char::is_whitespace) || tag.contains("..") || tag.starts_with('-') {
            return Some("A tag cannot contain spaces or \"..\", or start with a dash.");
        }
        if self.target.trim().starts_with('-') {
            return Some("The target cannot start with a dash.");
        }
        None
    }
}

/// Arguments of `gh release create` (after `gh`); the notes go to
/// standard input (`--notes-file -`).
pub fn create_args(full_name: &str, r: &NewRelease) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "release".into(),
        "create".into(),
        r.tag.trim().into(),
        "--repo".into(),
        full_name.into(),
    ];
    if !r.target.trim().is_empty() {
        a.push("--target".into());
        a.push(r.target.trim().into());
    }
    a.push("--title".into());
    a.push(if r.title.trim().is_empty() {
        r.tag.trim().into()
    } else {
        r.title.trim().into()
    });
    a.push("--notes-file".into());
    a.push("-".into());
    if r.draft {
        a.push("--draft".into());
    }
    if r.prerelease {
        a.push("--prerelease".into());
    }
    if !r.latest {
        a.push("--latest=false".into());
    }
    for f in &r.files {
        a.push(f.clone());
    }
    a
}

/// Run `gh release create` with the notes on standard input. Returns the
/// release URL gh prints.
pub fn create(gh: &Gh, r: &NewRelease, calls: &mut Vec<ApiCall>) -> Result<String> {
    if let Some(p) = r.problem() {
        return Err(Error::new(p));
    }
    for f in &r.files {
        if !Path::new(f).is_file() {
            return Err(Error::new(format!("{f} does not exist any more")));
        }
    }
    let args = create_args(&gh.full_name(), r);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    gh.cli(&refs, Some(r.notes.as_bytes()), calls)
}

/// Arguments of `gh release download` for one asset into `dir`.
pub fn download_args(full_name: &str, tag: &str, asset: &str, dir: &str) -> Vec<String> {
    vec![
        "release".into(),
        "download".into(),
        tag.into(),
        "--repo".into(),
        full_name.into(),
        "-p".into(),
        asset.into(),
        "-D".into(),
        dir.into(),
    ]
}

/// The previous published release of `releases[i]` in list order (newest
/// first): the next non-draft entry.
pub fn previous_of(releases: &[Release], i: usize) -> Option<&Release> {
    releases.iter().skip(i + 1).find(|r| !r.draft)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gh() -> Gh {
        Gh::new(".", "o", "r")
    }

    #[test]
    fn parses_a_trimmed_real_release() {
        let raw = r##"[{"id":374801977,"tag_name":"8.5.0","target_commitish":"main","name":"8.5.0",
            "draft":false,"immutable":false,"prerelease":false,"created_at":"2026-08-22T04:41:37Z",
            "published_at":"2026-08-27T04:00:53Z","html_url":"https://github.com/pallets/click/releases/tag/8.5.0",
            "author":{"login":"github-actions[bot]","type":"Bot"},
            "assets":[{"id":524661931,"name":"click-8.5.0-py3-none-any.whl","label":"","content_type":"application/octet-stream",
              "state":"uploaded","size":125251,"download_count":60,"created_at":"2026-08-22T04:42:07Z",
              "browser_download_url":"https://github.com/pallets/click/releases/download/8.5.0/click-8.5.0-py3-none-any.whl"},
              {"id":2,"name":"click-8.5.0.tar.gz","size":300000,"download_count":15}],
            "body":"Changes:\n- one"}]"##;
        let r: Vec<Release> = serde_json::from_str(raw).unwrap();
        assert_eq!(r[0].title(), "8.5.0");
        assert_eq!(r[0].downloads(), 75);
        assert_eq!(r[0].asset_bytes(), 425_251);
        assert_eq!(r[0].author_login(), "github-actions[bot]");
        assert_eq!(r[0].published(), parse_time("2026-08-27T04:00:53Z"));
        // Missing fields are tolerated.
        let r: Release = serde_json::from_str(r#"{"id":1,"tag_name":"v1","name":null}"#).unwrap();
        assert_eq!(r.title(), "v1");
        assert!(r.time().is_none());
    }

    #[test]
    fn list_and_latest_endpoints() {
        assert_eq!(
            list_endpoint(&gh(), 2),
            "repos/o/r/releases?per_page=30&page=2"
        );
        assert_eq!(
            list_endpoint(&gh(), 0),
            "repos/o/r/releases?per_page=30&page=1"
        );
    }

    #[test]
    fn generate_notes_body() {
        let r = generate_notes_request(&gh(), "v1.2.0", "main", Some("v1.1.0"));
        assert_eq!(r.method.as_str(), "POST");
        assert_eq!(r.endpoint, "repos/o/r/releases/generate-notes");
        assert_eq!(
            r.body,
            Some(
                json!({"tag_name": "v1.2.0", "target_commitish": "main", "previous_tag_name": "v1.1.0"})
            )
        );
        let r = generate_notes_request(&gh(), "v1.0.0", "", None);
        assert_eq!(r.body, Some(json!({"tag_name": "v1.0.0"})));
    }

    #[test]
    fn edit_and_delete_requests() {
        let e = ReleaseEdit {
            name: "One".into(),
            body: "notes".into(),
            draft: false,
            prerelease: true,
            make_latest: Some(false),
        };
        let r = edit_request(&gh(), 42, &e);
        assert_eq!(r.method.as_str(), "PATCH");
        assert_eq!(r.endpoint, "repos/o/r/releases/42");
        assert_eq!(
            r.body,
            Some(
                json!({"name": "One", "body": "notes", "draft": false, "prerelease": true, "make_latest": "false"})
            )
        );
        let e = ReleaseEdit {
            make_latest: None,
            ..e
        };
        assert!(
            edit_request(&gh(), 42, &e)
                .body
                .unwrap()
                .get("make_latest")
                .is_none()
        );
        let d = delete_request(&gh(), 42);
        assert_eq!(d.method.as_str(), "DELETE");
        assert_eq!(d.endpoint, "repos/o/r/releases/42");
        assert_eq!(d.body, None);
        let t = delete_tag_request(&gh(), "release/v1.0 rc");
        assert_eq!(t.endpoint, "repos/o/r/git/refs/tags/release/v1.0%20rc");
    }

    #[test]
    fn create_command_line() {
        let mut r = NewRelease {
            tag: "v1.2.0".into(),
            target: "main".into(),
            title: "Version 1.2".into(),
            notes: "secret-free notes".into(),
            draft: true,
            prerelease: true,
            latest: false,
            files: vec!["dist/app.zip".into(), "C:/out/app setup.exe".into()],
        };
        assert_eq!(
            create_args("o/r", &r),
            vec![
                "release",
                "create",
                "v1.2.0",
                "--repo",
                "o/r",
                "--target",
                "main",
                "--title",
                "Version 1.2",
                "--notes-file",
                "-",
                "--draft",
                "--prerelease",
                "--latest=false",
                "dist/app.zip",
                "C:/out/app setup.exe"
            ]
        );
        // The notes never reach the command line.
        assert!(
            !create_args("o/r", &r)
                .iter()
                .any(|a| a.contains("secret-free"))
        );
        r.draft = false;
        r.prerelease = false;
        r.latest = true;
        r.files.clear();
        r.title.clear();
        r.target.clear();
        assert_eq!(
            create_args("o/r", &r),
            vec![
                "release",
                "create",
                "v1.2.0",
                "--repo",
                "o/r",
                "--title",
                "v1.2.0",
                "--notes-file",
                "-"
            ]
        );
    }

    #[test]
    fn create_validation() {
        let mut r = NewRelease::default();
        assert!(r.problem().is_some());
        r.tag = "v 1".into();
        assert!(r.problem().is_some());
        r.tag = "--draft".into();
        assert!(r.problem().is_some());
        r.tag = "v1.0.0".into();
        assert!(r.problem().is_none());
        r.target = "-x".into();
        assert!(r.problem().is_some());
    }

    #[test]
    fn download_command_line() {
        assert_eq!(
            download_args("o/r", "v1", "app.zip", "C:/Downloads"),
            vec![
                "release",
                "download",
                "v1",
                "--repo",
                "o/r",
                "-p",
                "app.zip",
                "-D",
                "C:/Downloads"
            ]
        );
    }

    #[test]
    fn previous_skips_drafts() {
        let mk = |tag: &str, draft: bool| Release {
            tag_name: tag.into(),
            draft,
            ..Default::default()
        };
        let list = vec![mk("v3", false), mk("v2-draft", true), mk("v1", false)];
        assert_eq!(previous_of(&list, 0).unwrap().tag_name, "v1");
        assert!(previous_of(&list, 2).is_none());
    }
}
