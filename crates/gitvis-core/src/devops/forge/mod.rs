//! GitLab (through `glab api`) and Azure DevOps (through `az`): merge
//! requests and pipelines for repositories whose remote is not on GitHub.
//!
//! [`detect`] recognises the remote; the adapters live in the submodules.

/// Which forge a remote points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ForgeKind {
    GitLab,
    Azure,
}

impl ForgeKind {
    pub fn label(self) -> &'static str {
        match self {
            ForgeKind::GitLab => "GitLab",
            ForgeKind::Azure => "Azure DevOps",
        }
    }

    /// What the forge calls a pull request.
    pub fn review_noun(self) -> &'static str {
        match self {
            ForgeKind::GitLab => "merge request",
            ForgeKind::Azure => "pull request",
        }
    }
}

/// A remote on GitLab or Azure DevOps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForgeRemote {
    pub kind: ForgeKind,
    /// `gitlab.com`, `gitlab.example.com`, `dev.azure.com`.
    pub host: String,
    /// GitLab: the full project path (`group/subgroup/name`).
    /// Azure: `org/project/repo`.
    pub path: String,
    /// Azure organisation; `None` on GitLab.
    pub org: Option<String>,
    /// Azure project; `None` on GitLab.
    pub project: Option<String>,
    /// Repository (GitLab: last path segment).
    pub repo: String,
    /// The repository's web page.
    pub web_url: String,
}

fn strip_git(s: &str) -> &str {
    s.trim_end_matches('/').trim_end_matches(".git")
}

/// `(host, path)` from `https://host/path`, `ssh://git@host:port/path`,
/// `git@host:path`.
fn host_and_path(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("ssh://"))
        .or_else(|| url.strip_prefix("git+ssh://"))
    {
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit('@').next()?;
        let host = host.split(':').next()?;
        return Some((host.to_ascii_lowercase(), strip_git(path).to_owned()));
    }
    // scp-like: user@host:path
    let (user_host, path) = url.split_once(':')?;
    if path.starts_with("//") {
        return None;
    }
    let host = user_host.rsplit('@').next()?;
    Some((host.to_ascii_lowercase(), strip_git(path).to_owned()))
}

/// Recognise a GitLab or Azure DevOps remote URL.
pub fn detect(url: &str) -> Option<ForgeRemote> {
    let (host, path) = host_and_path(url)?;
    if path.is_empty() {
        return None;
    }
    // Azure DevOps.
    if host == "dev.azure.com" {
        // org/project/_git/repo
        let parts: Vec<&str> = path.split('/').collect();
        if let [org, project, "_git", repo, ..] = parts.as_slice() {
            return Some(azure(org, project, repo));
        }
        return None;
    }
    if host == "ssh.dev.azure.com" || host == "vs-ssh.visualstudio.com" {
        // v3/org/project/repo
        let parts: Vec<&str> = path.split('/').collect();
        if let ["v3", org, project, repo, ..] = parts.as_slice() {
            return Some(azure(org, project, repo));
        }
        return None;
    }
    if let Some(org) = host.strip_suffix(".visualstudio.com") {
        // [DefaultCollection/]project/_git/repo
        let parts: Vec<&str> = path
            .split('/')
            .filter(|p| !p.eq_ignore_ascii_case("DefaultCollection"))
            .collect();
        if let [project, "_git", repo, ..] = parts.as_slice() {
            return Some(azure(org, project, repo));
        }
        return None;
    }
    // GitLab: gitlab.com or a self-hosted host with "gitlab" in its name.
    if host == "gitlab.com" || host.split('.').any(|label| label.contains("gitlab")) {
        let path = path.trim_matches('/').to_owned();
        if !path.contains('/') {
            return None;
        }
        let repo = path.rsplit('/').next().unwrap_or(&path).to_owned();
        return Some(ForgeRemote {
            kind: ForgeKind::GitLab,
            web_url: format!("https://{host}/{path}"),
            host,
            path,
            org: None,
            project: None,
            repo,
        });
    }
    None
}

fn decode_segment(s: &str) -> String {
    s.replace("%20", " ")
}

fn azure(org: &str, project: &str, repo: &str) -> ForgeRemote {
    let (org, project, repo) = (
        decode_segment(org),
        decode_segment(project),
        decode_segment(repo),
    );
    ForgeRemote {
        kind: ForgeKind::Azure,
        host: "dev.azure.com".to_owned(),
        path: format!("{org}/{project}/{repo}"),
        web_url: format!(
            "https://dev.azure.com/{}/{}/_git/{}",
            org.replace(' ', "%20"),
            project.replace(' ', "%20"),
            repo.replace(' ', "%20")
        ),
        org: Some(org),
        project: Some(project),
        repo,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gitlab_urls() {
        for url in [
            "https://gitlab.com/group/sub/project.git",
            "git@gitlab.com:group/sub/project.git",
            "ssh://git@gitlab.com:2222/group/sub/project.git",
        ] {
            let f = detect(url).unwrap_or_else(|| panic!("{url}"));
            assert_eq!(f.kind, ForgeKind::GitLab);
            assert_eq!(f.host, "gitlab.com");
            assert_eq!(f.path, "group/sub/project");
            assert_eq!(f.repo, "project");
            assert_eq!(f.web_url, "https://gitlab.com/group/sub/project");
        }
        let own = detect("https://gitlab.example.org/team/app").unwrap();
        assert_eq!(own.host, "gitlab.example.org");
        assert_eq!(own.path, "team/app");
    }

    #[test]
    fn azure_urls() {
        for url in [
            "https://dev.azure.com/acme/Web%20Shop/_git/frontend",
            "https://acme@dev.azure.com/acme/Web%20Shop/_git/frontend",
            "git@ssh.dev.azure.com:v3/acme/Web%20Shop/frontend",
            "https://acme.visualstudio.com/Web%20Shop/_git/frontend",
            "https://acme.visualstudio.com/DefaultCollection/Web%20Shop/_git/frontend",
        ] {
            let f = detect(url).unwrap_or_else(|| panic!("{url}"));
            assert_eq!(f.kind, ForgeKind::Azure, "{url}");
            assert_eq!(f.org.as_deref(), Some("acme"));
            assert_eq!(f.project.as_deref(), Some("Web Shop"));
            assert_eq!(f.repo, "frontend");
            assert_eq!(
                f.web_url,
                "https://dev.azure.com/acme/Web%20Shop/_git/frontend"
            );
        }
    }

    #[test]
    fn others_are_not_forges() {
        assert!(detect("https://github.com/o/r.git").is_none());
        assert!(detect("git@bitbucket.org:o/r.git").is_none());
        assert!(detect("/local/path/repo").is_none());
        assert!(detect("https://gitlab.com/onlyone").is_none());
    }
}
