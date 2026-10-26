//! Software bill of materials: packages from the lockfiles (through the
//! OSINT lockfile parsers), package URLs, CycloneDX 1.5 and SPDX 2.3 JSON,
//! GitHub's dependency-graph SBOM and a comparison of the two.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{Value, json};

use crate::Result;
use crate::devops::gh::Gh;
use crate::github::ApiCall;
use crate::osint::{GitDep, Package, Supply, parse_manifest};

/// File names the lockfile parsers read.
const MANIFESTS: [&str; 14] = [
    "package.json",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "Cargo.lock",
    "Cargo.toml",
    "Pipfile.lock",
    "go.mod",
    "Gemfile",
    "Gemfile.lock",
    "composer.lock",
    "pyproject.toml",
    "constraints.txt",
];

fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// Whether a tracked path is a manifest or lockfile the parsers read.
pub fn is_manifest(path: &str) -> bool {
    let b = basename(path);
    MANIFESTS.contains(&b) || (b.starts_with("requirements") && b.ends_with(".txt"))
}

/// Vendored or third-party copies are not the project's dependencies.
pub fn is_vendored(path: &str) -> bool {
    let l = path.to_lowercase();
    l.contains("node_modules/")
        || l.contains("/vendor/")
        || l.starts_with("vendor/")
        || l.contains("third_party/")
        || l.contains("/.cargo/")
}

/// The packages of the working tree.
#[derive(Clone, Debug, Default)]
pub struct LocalSbom {
    /// Sorted by ecosystem, name, version; one entry per lockfile occurrence.
    pub packages: Vec<Package>,
    /// `(file, packages)` per file read.
    pub lockfiles: Vec<(String, usize)>,
    /// Dependencies pulled straight from git (no registry version).
    pub git_deps: Vec<GitDep>,
    pub manifests_read: usize,
    pub bytes_read: usize,
    pub elapsed_ms: u64,
}

impl LocalSbom {
    /// Distinct `(ecosystem, name, version)` triples.
    pub fn unique(&self) -> Vec<&Package> {
        let mut seen = BTreeSet::new();
        self.packages
            .iter()
            .filter(|p| seen.insert((p.ecosystem, p.name.as_str(), p.version.as_str())))
            .collect()
    }

    /// `(ecosystem, distinct packages)`, largest first.
    pub fn by_ecosystem(&self) -> Vec<(&'static str, usize)> {
        let mut m: BTreeMap<&'static str, BTreeSet<(&str, &str)>> = BTreeMap::new();
        for p in &self.packages {
            m.entry(p.ecosystem)
                .or_default()
                .insert((p.name.as_str(), p.version.as_str()));
        }
        let mut v: Vec<(&'static str, usize)> = m.into_iter().map(|(k, s)| (k, s.len())).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        v
    }
}

/// Read every tracked manifest and lockfile (files over 20 MB skipped).
pub fn local_sbom(workdir: &Path, tracked: &[String]) -> LocalSbom {
    let started = std::time::Instant::now();
    let mut supply = Supply::default();
    let mut out = LocalSbom::default();
    for p in tracked.iter().filter(|p| is_manifest(p) && !is_vendored(p)) {
        let full = workdir.join(p);
        if let Ok(meta) = std::fs::metadata(&full)
            && meta.len() <= 20 * 1024 * 1024
            && let Ok(text) = std::fs::read_to_string(&full)
        {
            out.manifests_read += 1;
            out.bytes_read += text.len();
            parse_manifest(p, &text, &mut supply);
        }
    }
    let mut seen = BTreeSet::new();
    supply.packages.retain(|p| {
        seen.insert((
            p.ecosystem,
            p.name.clone(),
            p.version.clone(),
            p.file.clone(),
        ))
    });
    supply.packages.sort_by(|a, b| {
        a.ecosystem
            .cmp(b.ecosystem)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.version.cmp(&b.version))
            .then_with(|| a.file.cmp(&b.file))
    });
    supply.git_deps.dedup();
    out.packages = supply.packages;
    out.lockfiles = supply.lockfiles;
    out.git_deps = supply.git_deps;
    out.elapsed_ms = started.elapsed().as_millis() as u64;
    out
}

/// The purl type of an osv.dev ecosystem name.
pub fn purl_type(ecosystem: &str) -> &'static str {
    match ecosystem {
        "npm" => "npm",
        "crates.io" => "cargo",
        "PyPI" => "pypi",
        "Go" => "golang",
        "RubyGems" => "gem",
        "Packagist" => "composer",
        _ => "generic",
    }
}

/// Percent-encode a purl component (keeps `A-Za-z0-9 . - _ ~`).
fn purl_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' | b'~' => {
                out.push(b as char)
            }
            b'/' if keep_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The package URL (`pkg:npm/%40scope/name@1.0.0`, `pkg:cargo/serde@1.0`,
/// `pkg:golang/github.com/x/y@v1.2.3`, `pkg:pypi/django@4.2`, …).
pub fn purl(p: &Package) -> String {
    let t = purl_type(p.ecosystem);
    let (name, version) = match t {
        // PEP 503 normalised, lower-case.
        "pypi" => (p.name.to_lowercase().replace('_', "-"), p.version.clone()),
        // Go versions keep their `v`.
        "golang" => (
            p.name.clone(),
            if p.version.starts_with('v') {
                p.version.clone()
            } else {
                format!("v{}", p.version)
            },
        ),
        _ => (p.name.clone(), p.version.clone()),
    };
    let path = match t {
        "npm" => match name.strip_prefix('@').and_then(|n| n.split_once('/')) {
            Some((scope, n)) => {
                format!("%40{}/{}", purl_encode(scope, false), purl_encode(n, false))
            }
            None => purl_encode(&name, false),
        },
        // Namespaced types keep their slashes.
        "golang" | "composer" => purl_encode(&name, true),
        _ => purl_encode(&name, false),
    };
    format!("pkg:{t}/{path}@{}", purl_encode(&version, false))
}

/// `(type, name, version)` of a purl; npm scopes come back as `@scope/name`.
pub fn parse_purl(purl: &str) -> Option<(String, String, String)> {
    let rest = purl.strip_prefix("pkg:")?;
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let (t, path) = rest.split_once('/')?;
    let (path, version) = match path.rfind('@') {
        Some(i) if i > 0 => (&path[..i], &path[i + 1..]),
        _ => (path, ""),
    };
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let decode = |s: &str| -> String {
        let b = s.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%'
                && i + 2 < b.len()
                && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
            {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
            out.push(b[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    };
    Some((t.to_ascii_lowercase(), decode(path), decode(version)))
}

/// A CycloneDX 1.5 document. `serial` is a UUID, `timestamp` ISO 8601 UTC.
pub fn cyclonedx(
    root_name: &str,
    packages: &[Package],
    serial: &str,
    timestamp: &str,
    tool_version: &str,
) -> Value {
    let mut seen = BTreeSet::new();
    let components: Vec<Value> = packages
        .iter()
        .filter_map(|p| {
            let purl = purl(p);
            if !seen.insert(purl.clone()) {
                return None;
            }
            let files: Vec<&str> = packages
                .iter()
                .filter(|q| {
                    q.ecosystem == p.ecosystem && q.name == p.name && q.version == p.version
                })
                .map(|q| q.file.as_str())
                .collect();
            Some(json!({
                "type": "library",
                "bom-ref": purl,
                "name": p.name,
                "version": p.version,
                "purl": purl,
                "properties": files
                    .iter()
                    .map(|f| json!({"name": "gitvis:lockfile", "value": f}))
                    .collect::<Vec<_>>(),
            }))
        })
        .collect();
    json!({
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "serialNumber": format!("urn:uuid:{serial}"),
        "version": 1,
        "metadata": {
            "timestamp": timestamp,
            "tools": {
                "components": [{
                    "type": "application",
                    "name": "gitvis",
                    "version": tool_version,
                }]
            },
            "component": {
                "type": "application",
                "bom-ref": format!("root:{root_name}"),
                "name": root_name,
            }
        },
        "components": components,
        "dependencies": [{
            "ref": format!("root:{root_name}"),
            "dependsOn": seen.iter().collect::<Vec<_>>(),
        }],
    })
}

/// An SPDX identifier made of `[A-Za-z0-9.-]`.
fn spdx_id(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// An SPDX 2.3 JSON document. `namespace_id` makes the document namespace
/// unique (a UUID).
pub fn spdx(
    root_name: &str,
    packages: &[Package],
    namespace_id: &str,
    timestamp: &str,
    tool_version: &str,
) -> Value {
    let root_id = "SPDXRef-Repository".to_owned();
    let mut ids = BTreeSet::new();
    let mut pkgs = vec![json!({
        "SPDXID": root_id,
        "name": root_name,
        "downloadLocation": "NOASSERTION",
        "filesAnalyzed": false,
        "licenseConcluded": "NOASSERTION",
        "licenseDeclared": "NOASSERTION",
        "copyrightText": "NOASSERTION",
        "primaryPackagePurpose": "APPLICATION",
    })];
    let mut rels = vec![json!({
        "spdxElementId": "SPDXRef-DOCUMENT",
        "relationshipType": "DESCRIBES",
        "relatedSpdxElement": root_id,
    })];
    let mut seen = BTreeSet::new();
    for p in packages {
        let purl = purl(p);
        if !seen.insert(purl.clone()) {
            continue;
        }
        let mut id = format!(
            "SPDXRef-Package-{}-{}-{}",
            purl_type(p.ecosystem),
            spdx_id(&p.name),
            spdx_id(&p.version)
        );
        // Keep identifiers unique after the character mapping.
        let mut n = 2;
        while !ids.insert(id.clone()) {
            id = format!("{id}-{n}");
            n += 1;
        }
        let files: Vec<String> = packages
            .iter()
            .filter(|q| q.ecosystem == p.ecosystem && q.name == p.name && q.version == p.version)
            .map(|q| q.file.clone())
            .collect();
        pkgs.push(json!({
            "SPDXID": id,
            "name": p.name,
            "versionInfo": p.version,
            "downloadLocation": "NOASSERTION",
            "filesAnalyzed": false,
            "licenseConcluded": "NOASSERTION",
            "licenseDeclared": "NOASSERTION",
            "copyrightText": "NOASSERTION",
            "sourceInfo": format!("Locked in {}", files.join(", ")),
            "externalRefs": [{
                "referenceCategory": "PACKAGE-MANAGER",
                "referenceType": "purl",
                "referenceLocator": purl,
            }],
        }));
        rels.push(json!({
            "spdxElementId": root_id,
            "relationshipType": "DEPENDS_ON",
            "relatedSpdxElement": id,
        }));
    }
    json!({
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": format!("{root_name} SBOM"),
        "documentNamespace": format!("https://spdx.org/spdxdocs/gitvis/{}-{namespace_id}", spdx_id(root_name)),
        "creationInfo": {
            "created": timestamp,
            "creators": [format!("Tool: gitvis-{tool_version}")],
        },
        "packages": pkgs,
        "relationships": rels,
    })
}

/// One package of GitHub's SBOM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GhPackage {
    /// GitHub's display name, e.g. `npm:lodash`.
    pub name: String,
    pub version: String,
    pub purl: Option<String>,
    pub license: Option<String>,
}

/// GitHub's dependency-graph SBOM (SPDX 2.3).
#[derive(Clone, Debug, Default)]
pub struct GhSbom {
    pub raw: Value,
    pub name: String,
    pub created: String,
    pub packages: Vec<GhPackage>,
}

pub fn parse_gh_sbom(body: &[u8]) -> Result<GhSbom> {
    let v: Value = crate::devops::gh::decode(body, "dependency graph SBOM")?;
    let sbom = v.get("sbom").cloned().unwrap_or(v);
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    let packages = sbom
        .get("packages")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|p| GhPackage {
                    name: s(p, "name"),
                    version: s(p, "versionInfo"),
                    purl: p
                        .get("externalRefs")
                        .and_then(Value::as_array)
                        .and_then(|refs| {
                            refs.iter().find(|r| {
                                r.get("referenceType").and_then(Value::as_str) == Some("purl")
                            })
                        })
                        .and_then(|r| r.get("referenceLocator").and_then(Value::as_str))
                        .map(str::to_owned),
                    license: p
                        .get("licenseConcluded")
                        .or_else(|| p.get("licenseDeclared"))
                        .and_then(Value::as_str)
                        .filter(|l| *l != "NOASSERTION")
                        .map(str::to_owned),
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(GhSbom {
        name: s(&sbom, "name"),
        created: sbom
            .get("creationInfo")
            .map(|c| s(c, "created"))
            .unwrap_or_default(),
        packages,
        raw: sbom,
    })
}

/// `GET dependency-graph/sbom`.
pub fn fetch_gh_sbom(gh: &Gh, calls: &mut Vec<ApiCall>) -> Result<GhSbom> {
    parse_gh_sbom(&gh.get(&gh.path("dependency-graph/sbom"), calls)?)
}

/// Local packages against GitHub's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SbomDiff {
    /// Packages present on both sides with the same versions.
    pub matched: usize,
    /// `(type, name, versions)` only in the lockfiles.
    pub only_local: Vec<(String, String, Vec<String>)>,
    /// `(type, name, versions)` only on GitHub.
    pub only_github: Vec<(String, String, Vec<String>)>,
    /// `(type, name, local versions, GitHub versions)`.
    pub mismatched: Vec<(String, String, Vec<String>, Vec<String>)>,
    /// GitHub lists a range (from a manifest without a lockfile).
    pub ranges: usize,
    /// Ecosystems on GitHub that gitvis does not read: `(type, packages)`.
    pub unsupported: Vec<(String, usize)>,
}

const SUPPORTED: [&str; 6] = ["npm", "cargo", "pypi", "golang", "gem", "composer"];

fn norm_version(v: &str) -> String {
    v.trim().trim_start_matches('v').to_owned()
}

fn is_range(v: &str) -> bool {
    v.is_empty() || v.contains(['^', '~', '<', '>', '*', '=', ' ', '|', ','])
}

fn norm_name(t: &str, name: &str) -> String {
    if t == "pypi" {
        name.to_lowercase().replace('_', "-")
    } else {
        name.to_owned()
    }
}

/// Compare by purl type and name, then versions.
pub fn compare(local: &[Package], github: &[GhPackage]) -> SbomDiff {
    let mut l: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for p in local {
        let t = purl_type(p.ecosystem).to_owned();
        l.entry((t.clone(), norm_name(&t, &p.name)))
            .or_default()
            .insert(norm_version(&p.version));
    }
    let mut g: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    let mut unsupported: BTreeMap<String, usize> = BTreeMap::new();
    let mut out = SbomDiff::default();
    for p in github {
        let Some((t, name, version)) = p.purl.as_deref().and_then(parse_purl) else {
            continue;
        };
        if !SUPPORTED.contains(&t.as_str()) {
            *unsupported.entry(t).or_insert(0) += 1;
            continue;
        }
        let version = if version.is_empty() {
            p.version.clone()
        } else {
            version
        };
        if is_range(&version) {
            out.ranges += 1;
        }
        g.entry((t.clone(), norm_name(&t, &name)))
            .or_default()
            .insert(norm_version(&version));
    }
    for (k, lv) in &l {
        match g.get(k) {
            None => out
                .only_local
                .push((k.0.clone(), k.1.clone(), lv.iter().cloned().collect())),
            Some(gv) => {
                let exact: BTreeSet<&String> = gv.iter().filter(|v| !is_range(v)).collect();
                if exact.is_empty() || lv.iter().collect::<BTreeSet<_>>() == exact {
                    out.matched += 1;
                } else {
                    out.mismatched.push((
                        k.0.clone(),
                        k.1.clone(),
                        lv.iter().cloned().collect(),
                        gv.iter().cloned().collect(),
                    ));
                }
            }
        }
    }
    for (k, gv) in &g {
        if !l.contains_key(k) {
            out.only_github
                .push((k.0.clone(), k.1.clone(), gv.iter().cloned().collect()));
        }
    }
    out.unsupported = unsupported.into_iter().collect();
    out.unsupported.sort_by_key(|a| std::cmp::Reverse(a.1));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(eco: &'static str, name: &str, version: &str, file: &str) -> Package {
        Package {
            ecosystem: eco,
            name: name.into(),
            version: version.into(),
            file: file.into(),
        }
    }

    #[test]
    fn purls() {
        assert_eq!(
            purl(&pkg("npm", "@babel/core", "7.22.0", "")),
            "pkg:npm/%40babel/core@7.22.0"
        );
        assert_eq!(
            purl(&pkg("npm", "lodash", "4.17.21", "")),
            "pkg:npm/lodash@4.17.21"
        );
        assert_eq!(
            purl(&pkg("crates.io", "serde", "1.0.200", "")),
            "pkg:cargo/serde@1.0.200"
        );
        assert_eq!(
            purl(&pkg("Go", "github.com/spf13/cobra", "1.8.0", "")),
            "pkg:golang/github.com/spf13/cobra@v1.8.0"
        );
        assert_eq!(
            purl(&pkg("PyPI", "Django_REST", "3.0", "")),
            "pkg:pypi/django-rest@3.0"
        );
        assert_eq!(
            purl(&pkg("Packagist", "symfony/console", "6.4.1", "")),
            "pkg:composer/symfony/console@6.4.1"
        );
        assert_eq!(
            purl(&pkg("RubyGems", "rails", "7.1.0", "")),
            "pkg:gem/rails@7.1.0"
        );
        assert_eq!(
            purl(&pkg("crates.io", "x", "1.0.0+build", "")),
            "pkg:cargo/x@1.0.0%2Bbuild"
        );
        assert_eq!(
            parse_purl("pkg:npm/%40babel/core@7.22.0"),
            Some(("npm".into(), "@babel/core".into(), "7.22.0".into()))
        );
        assert_eq!(
            parse_purl("pkg:golang/github.com/x/y@v1.2.3?type=module"),
            Some(("golang".into(), "github.com/x/y".into(), "v1.2.3".into()))
        );
        assert_eq!(
            parse_purl("pkg:githubactions/actions/checkout@4.*.*"),
            Some((
                "githubactions".into(),
                "actions/checkout".into(),
                "4.*.*".into()
            ))
        );
        assert_eq!(parse_purl("nope"), None);
    }

    fn sample() -> Vec<Package> {
        vec![
            pkg("npm", "@babel/core", "7.22.0", "web/package-lock.json"),
            pkg("npm", "@babel/core", "7.22.0", "docs/package-lock.json"),
            pkg("crates.io", "serde", "1.0.200", "Cargo.lock"),
        ]
    }

    #[test]
    fn cyclonedx_required_fields() {
        let d = cyclonedx(
            "my-repo",
            &sample(),
            "0f8fad5b-d9cb-469f-a165-70867728950e",
            "2026-10-26T12:00:00Z",
            "0.1.0",
        );
        assert_eq!(d["bomFormat"], "CycloneDX");
        assert_eq!(d["specVersion"], "1.5");
        assert_eq!(d["version"], 1);
        assert_eq!(
            d["serialNumber"],
            "urn:uuid:0f8fad5b-d9cb-469f-a165-70867728950e"
        );
        assert_eq!(d["metadata"]["timestamp"], "2026-10-26T12:00:00Z");
        let comps = d["components"].as_array().unwrap();
        // The duplicate across two lockfiles is one component with two files.
        assert_eq!(comps.len(), 2);
        for c in comps {
            assert_eq!(c["type"], "library");
            assert!(c["name"].is_string());
            assert!(c["purl"].as_str().unwrap().starts_with("pkg:"));
            assert_eq!(c["bom-ref"], c["purl"]);
        }
        let babel = comps.iter().find(|c| c["name"] == "@babel/core").unwrap();
        assert_eq!(babel["purl"], "pkg:npm/%40babel/core@7.22.0");
        assert_eq!(babel["properties"].as_array().unwrap().len(), 2);
        assert_eq!(
            d["dependencies"][0]["dependsOn"].as_array().unwrap().len(),
            2
        );
    }

    #[test]
    fn spdx_required_fields() {
        let d = spdx(
            "my repo",
            &sample(),
            "0f8fad5b-d9cb-469f-a165-70867728950e",
            "2026-10-26T12:00:00Z",
            "0.1.0",
        );
        assert_eq!(d["spdxVersion"], "SPDX-2.3");
        assert_eq!(d["dataLicense"], "CC0-1.0");
        assert_eq!(d["SPDXID"], "SPDXRef-DOCUMENT");
        assert!(d["name"].is_string());
        assert!(
            d["documentNamespace"]
                .as_str()
                .unwrap()
                .starts_with("https://spdx.org/spdxdocs/gitvis/my-repo-0f8fad5b")
        );
        assert_eq!(d["creationInfo"]["created"], "2026-10-26T12:00:00Z");
        assert_eq!(d["creationInfo"]["creators"][0], "Tool: gitvis-0.1.0");
        let pkgs = d["packages"].as_array().unwrap();
        assert_eq!(pkgs.len(), 3);
        let id_ok = |s: &str| {
            s.starts_with("SPDXRef-")
                && s["SPDXRef-".len()..]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        };
        for p in pkgs {
            assert!(id_ok(p["SPDXID"].as_str().unwrap()), "{}", p["SPDXID"]);
            assert!(p["name"].is_string());
            assert_eq!(p["downloadLocation"], "NOASSERTION");
            assert_eq!(p["filesAnalyzed"], false);
        }
        let babel = pkgs.iter().find(|p| p["name"] == "@babel/core").unwrap();
        assert_eq!(
            babel["externalRefs"][0],
            json!({"referenceCategory": "PACKAGE-MANAGER", "referenceType": "purl", "referenceLocator": "pkg:npm/%40babel/core@7.22.0"})
        );
        let rels = d["relationships"].as_array().unwrap();
        assert_eq!(rels[0]["relationshipType"], "DESCRIBES");
        assert_eq!(rels.len(), 3);
    }

    #[test]
    fn github_sbom_and_compare() {
        let body = br#"{"sbom":{"SPDXID":"SPDXRef-DOCUMENT","spdxVersion":"SPDX-2.3","name":"com.github.o/r",
          "creationInfo":{"created":"2026-10-26T10:00:00Z","creators":["Tool: GitHub.com-Dependency-Graph"]},
          "packages":[
            {"SPDXID":"SPDXRef-npm-babel-core","name":"npm:@babel/core","versionInfo":"7.21.0","licenseConcluded":"MIT",
             "externalRefs":[{"referenceCategory":"PACKAGE-MANAGER","referenceLocator":"pkg:npm/%40babel/core@7.21.0","referenceType":"purl"}]},
            {"SPDXID":"SPDXRef-npm-left-pad","name":"npm:left-pad","versionInfo":"1.3.0",
             "externalRefs":[{"referenceCategory":"PACKAGE-MANAGER","referenceLocator":"pkg:npm/left-pad@1.3.0","referenceType":"purl"}]},
            {"SPDXID":"SPDXRef-cargo-serde","name":"cargo:serde","versionInfo":"1.0.200",
             "externalRefs":[{"referenceCategory":"PACKAGE-MANAGER","referenceLocator":"pkg:cargo/serde@1.0.200","referenceType":"purl"}]},
            {"SPDXID":"SPDXRef-actions","name":"actions:actions/checkout","versionInfo":"4.*.*",
             "externalRefs":[{"referenceCategory":"PACKAGE-MANAGER","referenceLocator":"pkg:githubactions/actions/checkout@4.*.*","referenceType":"purl"}]},
            {"SPDXID":"SPDXRef-repo","name":"com.github.o/r","versionInfo":"main"}
          ]}}"#;
        let g = parse_gh_sbom(body).unwrap();
        assert_eq!(g.packages.len(), 5);
        assert_eq!(g.created, "2026-10-26T10:00:00Z");
        assert_eq!(g.packages[0].license.as_deref(), Some("MIT"));
        let mut local = sample();
        local.push(pkg("PyPI", "requests", "2.31.0", "requirements.txt"));
        let d = compare(&local, &g.packages);
        assert_eq!(d.matched, 1); // serde
        assert_eq!(
            d.only_local,
            vec![("pypi".into(), "requests".into(), vec!["2.31.0".into()])]
        );
        assert_eq!(
            d.only_github,
            vec![("npm".into(), "left-pad".into(), vec!["1.3.0".into()])]
        );
        assert_eq!(d.mismatched.len(), 1);
        assert_eq!(d.mismatched[0].1, "@babel/core");
        assert_eq!(d.unsupported, vec![("githubactions".into(), 1)]);
    }

    #[test]
    fn local_from_files() {
        let dir = std::env::temp_dir().join(format!("gitvis-govern-sbom-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("web")).unwrap();
        std::fs::write(
            dir.join("Cargo.lock"),
            "[[package]]\nname = \"serde\"\nversion = \"1.0.200\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("web/package-lock.json"),
            r#"{"lockfileVersion":3,"packages":{"":{"name":"web"},"node_modules/lodash":{"version":"4.17.21"}}}"#,
        )
        .unwrap();
        let tracked = vec![
            "Cargo.lock".to_string(),
            "web/package-lock.json".to_string(),
            "node_modules/x/package-lock.json".to_string(),
            "README.md".to_string(),
        ];
        let s = local_sbom(&dir, &tracked);
        assert_eq!(s.manifests_read, 2);
        assert_eq!(s.packages.len(), 2);
        assert_eq!(s.by_ecosystem().len(), 2);
        assert_eq!(s.unique().len(), 2);
        assert!(is_manifest("a/requirements-dev.txt"));
        assert!(!is_manifest("a/requirements.md"));
        assert!(is_vendored("vendor/x/go.mod"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
