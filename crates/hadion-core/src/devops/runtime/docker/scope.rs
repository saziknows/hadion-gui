//! Which containers and compose projects belong to the open repository.
//!
//! - A compose project belongs to it when its working directory
//!   (`com.docker.compose.project.working_dir`) or one of its config files
//!   is inside the repository's work dir. Paths are compared by
//!   [`path_key`]: `/` separators, no `\\?\` prefix, no trailing slash, and
//!   case-insensitive on Windows; [`real_key`] first resolves 8.3 short
//!   names (`C:\Users\RUNNER~1\…`) through the file system.
//! - A compose file of the repository that no project was started from is
//!   listed as "not started".
//! - A container without compose labels belongs to it when its image's
//!   source label names one of the repository's remotes, or its revision
//!   label names a commit of the repository.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::compose::RepoFiles;
use super::model::{ComposeLs, Container, Image};
use super::revision::{REVISION_LABELS, SOURCE_LABELS, source_matches};
use crate::devops::runtime::matching::RefIndex;

/// A path for comparison (see the module docs).
pub fn path_key(p: &str) -> String {
    let mut s = p.trim().replace('\\', "/");
    for prefix in ["//?/", "//./"] {
        if let Some(rest) = s.strip_prefix(prefix)
            && !rest.to_ascii_uppercase().starts_with("UNC/")
        {
            s = rest.to_owned();
        }
    }
    while s.contains("//") && !s.starts_with("//") {
        s = s.replace("//", "/");
    }
    while s.len() > 1 && s.ends_with('/') && !s.ends_with(":/") {
        s.pop();
    }
    if cfg!(windows) {
        s = s.to_lowercase();
    }
    s
}

/// [`path_key`] of the path's real name: 8.3 short names and symlinks
/// resolved when the path exists.
pub fn real_key(p: &str) -> String {
    let real = Path::new(p.trim())
        .canonicalize()
        .map(|r| r.to_string_lossy().into_owned())
        .unwrap_or_else(|_| p.to_owned());
    path_key(&real)
}

/// `key` is `root` or below it.
pub fn inside(key: &str, root: &str) -> bool {
    !root.is_empty()
        && (key == root || key.starts_with(&format!("{}/", root.trim_end_matches('/'))))
}

/// The repository as the rules see it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoScope {
    pub workdir: PathBuf,
    pub root: String,
    pub remotes: Vec<String>,
}

impl RepoScope {
    /// Touches the file system (one `canonicalize`).
    pub fn new(workdir: &Path, remotes: Vec<String>) -> Self {
        Self {
            root: real_key(&workdir.to_string_lossy()),
            workdir: workdir.to_path_buf(),
            remotes,
        }
    }

    /// The absolute path of a repository-relative file.
    pub fn abs(&self, rel: &str) -> String {
        self.workdir.join(rel).to_string_lossy().into_owned()
    }
}

/// A compose project and its containers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Project {
    pub name: String,
    pub working_dir: String,
    pub config_files: Vec<String>,
    /// Indexes into the listing's containers (by service, then number).
    pub containers: Vec<usize>,
    /// `compose ls`'s status (`running(2), exited(1)`), when it listed it.
    pub status: String,
    pub in_repo: bool,
    /// The repository compose file it was started from.
    pub file: Option<usize>,
}

impl Project {
    pub fn running(&self, all: &[Container]) -> usize {
        self.containers
            .iter()
            .filter(|i| all.get(**i).is_some_and(|c| c.state.running()))
            .count()
    }
}

/// The listing split by scope.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scoped {
    pub projects: Vec<Project>,
    /// Standalone containers of the repository, with why.
    pub standalone: Vec<(usize, String)>,
    /// Indexes into [`RepoFiles::compose`].
    pub not_started: Vec<usize>,
    pub other_projects: Vec<Project>,
    pub other_standalone: Vec<usize>,
}

impl Scoped {
    /// Indexes of the repository's containers.
    pub fn repo_containers(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self
            .projects
            .iter()
            .flat_map(|p| p.containers.iter().copied())
            .collect();
        v.extend(self.standalone.iter().map(|(i, _)| *i));
        v
    }

    pub fn project(&self, name: &str) -> Option<&Project> {
        self.projects
            .iter()
            .chain(&self.other_projects)
            .find(|p| p.name == name)
    }
}

fn image_of<'a>(images: &'a [Image], c: &Container) -> Option<&'a Image> {
    images.iter().find(|i| i.id == c.image_id)
}

/// Why a standalone container belongs to the repository, if it does.
fn standalone_reason(
    c: &Container,
    image: Option<&Image>,
    scope: &RepoScope,
    idx: Option<&RefIndex>,
) -> Option<String> {
    let get = |k: &str| {
        c.label(k)
            .or_else(|| image.and_then(|i| i.label(k)))
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    for k in SOURCE_LABELS {
        if let Some(src) = get(k)
            && source_matches(src, &scope.remotes)
        {
            return Some(format!("its image's source is {src}"));
        }
    }
    let idx = idx?;
    for k in REVISION_LABELS {
        if let Some(rev) = get(k)
            && let Some(id) = idx.commit_by_prefix(rev)
        {
            return Some(format!(
                "its image's revision label names commit {}",
                &id.to_string()[..7]
            ));
        }
    }
    None
}

/// Split containers, compose projects and the repository's compose files
/// by scope. `key` turns a path into a comparison key ([`real_key`] in the
/// app; [`path_key`] in tests).
pub fn classify(
    containers: &[Container],
    images: &[Image],
    ls: &[ComposeLs],
    files: &RepoFiles,
    scope: &RepoScope,
    idx: Option<&RefIndex>,
    key: &dyn Fn(&str) -> String,
) -> Scoped {
    let mut by_name: BTreeMap<String, Project> = BTreeMap::new();
    let mut standalone: Vec<usize> = Vec::new();
    for (i, c) in containers.iter().enumerate() {
        let Some(info) = &c.compose else {
            standalone.push(i);
            continue;
        };
        let p = by_name
            .entry(info.project.clone())
            .or_insert_with(|| Project {
                name: info.project.clone(),
                ..Project::default()
            });
        if p.working_dir.is_empty() {
            p.working_dir = info.working_dir.clone();
        }
        for f in &info.config_files {
            if !p.config_files.contains(f) {
                p.config_files.push(f.clone());
            }
        }
        p.containers.push(i);
    }
    for l in ls {
        let p = by_name.entry(l.name.clone()).or_insert_with(|| Project {
            name: l.name.clone(),
            ..Project::default()
        });
        p.status = l.status.clone();
        if p.config_files.is_empty() {
            p.config_files = l.config_files.clone();
        }
        if p.working_dir.is_empty()
            && let Some(dir) = l
                .config_files
                .first()
                .and_then(|f| Path::new(f).parent())
                .map(|d| d.to_string_lossy().into_owned())
        {
            p.working_dir = dir;
        }
    }
    let file_keys: Vec<String> = files
        .compose
        .iter()
        .map(|f| key(&scope.abs(&f.rel)))
        .collect();
    let mut covered = vec![false; files.compose.len()];
    let mut projects = Vec::new();
    let mut other_projects = Vec::new();
    for (_, mut p) in by_name {
        let config_keys: Vec<String> = p.config_files.iter().map(|f| key(f)).collect();
        p.in_repo = (!p.working_dir.is_empty() && inside(&key(&p.working_dir), &scope.root))
            || config_keys.iter().any(|k| inside(k, &scope.root));
        for (fi, fk) in file_keys.iter().enumerate() {
            if config_keys.contains(fk) {
                covered[fi] = true;
                if p.file.is_none() {
                    p.file = Some(fi);
                }
            }
        }
        let svc = |i: &usize| {
            containers[*i]
                .compose
                .as_ref()
                .map(|c| (c.oneoff, c.service.clone(), c.number.unwrap_or(0)))
                .unwrap_or_default()
        };
        p.containers.sort_by_key(svc);
        if p.in_repo {
            projects.push(p);
        } else {
            other_projects.push(p);
        }
    }
    let mut mine = Vec::new();
    let mut others = Vec::new();
    for i in standalone {
        let c = &containers[i];
        match standalone_reason(c, image_of(images, c), scope, idx) {
            Some(why) => mine.push((i, why)),
            None => others.push(i),
        }
    }
    Scoped {
        projects,
        standalone: mine,
        not_started: covered
            .iter()
            .enumerate()
            .filter(|(_, c)| !**c)
            .map(|(i, _)| i)
            .collect(),
        other_projects,
        other_standalone: others,
    }
}

#[cfg(test)]
mod tests {
    use super::super::compose::ComposeFile;
    use super::super::model::parse_inspect;
    use super::*;
    use crate::Oid;

    #[test]
    fn path_keys() {
        assert_eq!(path_key(r"\\?\C:\Repo\Shop\"), path_key("C:/Repo/Shop"));
        assert!(path_key(r"\\?\UNC\server\share").starts_with("//?/"));
        assert!(inside(&path_key(r"C:\repo\ops"), &path_key("C:/repo")));
        assert!(inside(&path_key("C:/repo"), &path_key(r"C:\repo\")));
        assert!(!inside(&path_key("C:/repository"), &path_key("C:/repo")));
        assert!(!inside("x", ""));
        if cfg!(windows) {
            assert_eq!(path_key(r"C:\Users\Me"), path_key("c:/users/me"));
        }
    }

    #[test]
    fn real_keys_resolve_short_names() {
        // The temp folder may be an 8.3 name (`RUNNER~1` on CI); its real key
        // equals the key of its canonical long name.
        let t = std::env::temp_dir();
        let long = t.canonicalize().unwrap_or(t.clone());
        assert_eq!(
            real_key(&t.to_string_lossy()),
            path_key(&long.to_string_lossy())
        );
        // A path that does not exist is compared as written.
        assert_eq!(real_key("C:/nowhere/at/all"), path_key("C:/nowhere/at/all"));
    }

    fn scope() -> RepoScope {
        RepoScope {
            workdir: PathBuf::from("C:/repo"),
            root: path_key("C:/repo"),
            remotes: vec!["git@github.com:acme/shop.git".into()],
        }
    }

    #[test]
    fn projects_files_and_standalone_containers() {
        let mut all = parse_inspect(include_str!("fixtures/inspect.json").as_bytes()).unwrap();
        let named = |all: &[Container], n: &str| all.iter().find(|c| c.name == n).unwrap().clone();
        let mut b = [0u8; 20];
        b[0] = 0xab;
        b[19] = 0xab;
        let commit = Oid::from_bytes(&b).unwrap();
        // A container of another project, and two standalone ones.
        let mut blog = named(&all, "shop-web-1");
        blog.id = "77".repeat(32);
        blog.name = "blog-web-1".into();
        let info = blog.compose.as_mut().unwrap();
        info.project = "blog".into();
        info.working_dir = r"D:\sites\blog".into();
        info.config_files = vec![r"D:\sites\blog\compose.yaml".into()];
        all.push(blog);
        let plain = named(&all, "never-started");
        let mut preview = plain.clone();
        preview.id = "88".repeat(32);
        preview.name = "api-preview".into();
        preview.labels.insert(
            "org.opencontainers.image.revision".into(),
            commit.to_string(),
        );
        all.push(preview);
        let files = RepoFiles {
            compose: vec![
                ComposeFile {
                    rel: "compose.yaml".into(),
                    overrides: vec!["compose.override.yaml".into()],
                },
                ComposeFile {
                    rel: "ops/compose.yaml".into(),
                    overrides: vec![],
                },
                ComposeFile {
                    rel: "extras/docker-compose.yml".into(),
                    overrides: vec![],
                },
            ],
            dockerfiles: vec![],
        };
        let ls = vec![ComposeLs {
            name: "shop".into(),
            status: "running(2), restarting(1), exited(1)".into(),
            config_files: vec![r"C:\repo\compose.yaml".into()],
        }];
        let idx = RefIndex::new([commit], vec![], vec![], vec![], None, None);
        let s = classify(&all, &[], &ls, &files, &scope(), Some(&idx), &path_key);
        let names: Vec<&str> = s.projects.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["devtools", "shop"]);
        let shop = s.project("shop").unwrap();
        assert_eq!(shop.status, "running(2), restarting(1), exited(1)");
        assert_eq!(shop.file, Some(0));
        assert_eq!(shop.containers.len(), 4);
        // Services in order, the one-off job last.
        let svc: Vec<&str> = shop
            .containers
            .iter()
            .map(|i| all[*i].compose.as_ref().unwrap().service.as_str())
            .collect();
        assert_eq!(svc, ["db", "web", "worker", "migrate"]);
        assert_eq!(shop.running(&all), 2);
        assert_eq!(s.project("devtools").unwrap().file, Some(1));
        assert_eq!(s.not_started, [2], "extras/docker-compose.yml");
        assert_eq!(s.other_projects.len(), 1);
        assert_eq!(s.other_projects[0].name, "blog");
        assert_eq!(s.standalone.len(), 1);
        assert_eq!(all[s.standalone[0].0].name, "api-preview");
        assert!(s.standalone[0].1.contains("commit ab00000"));
        assert_eq!(s.other_standalone.len(), 1, "never-started is not ours");
        assert_eq!(s.repo_containers().len(), 6);
        // The image source label is enough too.
        let mut src = plain;
        src.labels.insert(
            "org.opencontainers.image.source".into(),
            "https://github.com/acme/shop".into(),
        );
        let s = classify(
            &[src],
            &[],
            &[],
            &RepoFiles::default(),
            &scope(),
            None,
            &path_key,
        );
        assert_eq!(s.standalone.len(), 1);
    }
}
