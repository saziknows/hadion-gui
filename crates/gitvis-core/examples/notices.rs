//! Writes the third-party licence notices that ship with a release:
//!
//! ```text
//! cargo run -p gitvis-core --example notices -- [--target TRIPLE] [--out FILE]
//! ```
//!
//! Every crate compiled into gitvis for the target (normal dependencies,
//! as resolved in Cargo.lock; no dev or build dependencies), with its
//! licence expression, repository and the licence and notice files it
//! ships. Vendored C sources and fonts carry their own licences, which are
//! added explicitly (libgit2's `COPYING`, zlib, egui's default fonts,
//! libdbus). Identical texts are printed once and referenced afterwards.
//! `--target` defaults to the host (`rustc -vV`); `--out` to standard output.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// Licence files inside vendored sources, by crate name.
const VENDORED: &[(&str, &[&str])] = &[
    ("libgit2-sys", &["libgit2/COPYING"]),
    ("libz-sys", &["src/zlib/LICENSE"]),
    ("epaint_default_fonts", &["fonts/OFL.txt", "fonts/UFL.txt"]),
    ("libdbus-sys", &["vendor/dbus/COPYING"]),
];

struct Crate {
    name: String,
    version: String,
    license: String,
    repository: String,
    files: Vec<(String, String)>,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut target: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--target" => target = args.next(),
            "--out" => out = args.next().map(PathBuf::from),
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    let target = target.unwrap_or_else(host_triple);
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let crates = collect(&root, &target);
    let text = render(&crates, &target);
    match out {
        Some(p) => {
            std::fs::write(&p, text).unwrap_or_else(|e| panic!("writing {}: {e}", p.display()));
            // stdout: Windows PowerShell treats a native stderr line as an error.
            println!("{} crates -> {}", crates.len(), p.display());
        }
        None => print!("{text}"),
    }
}

fn host_triple() -> String {
    let out = Command::new("rustc")
        .arg("-vV")
        .output()
        .expect("running rustc -vV");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("host: ").map(str::to_owned))
        .expect("no host line in rustc -vV")
}

fn collect(root: &Path, target: &str) -> Vec<Crate> {
    let out = Command::new("cargo")
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--filter-platform",
            target,
        ])
        .current_dir(root)
        .output()
        .expect("running cargo metadata");
    assert!(
        out.status.success(),
        "cargo metadata: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: Value = serde_json::from_slice(&out.stdout).expect("cargo metadata JSON");

    let packages: HashMap<&str, &Value> = meta["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["id"].as_str().unwrap(), p))
        .collect();
    let nodes: HashMap<&str, &Value> = meta["resolve"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| (n["id"].as_str().unwrap(), n))
        .collect();
    let members: HashSet<&str> = meta["workspace_members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap())
        .collect();

    // Normal dependencies reachable from the workspace members.
    let mut seen: HashSet<&str> = HashSet::new();
    let mut stack: Vec<&str> = members.iter().copied().collect();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        for dep in nodes[id]["deps"].as_array().unwrap() {
            let normal = dep["dep_kinds"]
                .as_array()
                .unwrap()
                .iter()
                .any(|k| k["kind"].is_null());
            if normal {
                stack.push(dep["pkg"].as_str().unwrap());
            }
        }
    }

    let mut crates: Vec<Crate> = seen
        .into_iter()
        .filter(|id| !members.contains(id))
        .map(|id| {
            let p = packages[id];
            let name = p["name"].as_str().unwrap().to_owned();
            let dir = Path::new(p["manifest_path"].as_str().unwrap())
                .parent()
                .unwrap()
                .to_path_buf();
            let mut files = licence_files(&dir);
            if let Some(f) = p["license_file"].as_str()
                && !files.iter().any(|(n, _)| n == f)
                && let Ok(t) = read(&dir.join(f))
            {
                files.push((f.to_owned(), t));
            }
            for (_, extra) in VENDORED.iter().filter(|(c, _)| *c == name) {
                for rel in *extra {
                    if let Ok(t) = read(&dir.join(rel)) {
                        files.push(((*rel).to_owned(), t));
                    }
                }
            }
            Crate {
                version: p["version"].as_str().unwrap().to_owned(),
                license: p["license"]
                    .as_str()
                    .unwrap_or("(none declared)")
                    .to_owned(),
                repository: p["repository"].as_str().unwrap_or("").to_owned(),
                name,
                files,
            }
        })
        .collect();
    crates.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
    crates
}

/// Licence and notice files at the root of a crate.
fn licence_files(dir: &Path) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let lower = name.to_ascii_lowercase();
            let licence_like = [
                "license",
                "licence",
                "copying",
                "notice",
                "unlicense",
                "copyright",
            ]
            .iter()
            .any(|p| lower.starts_with(p));
            let source = [".rs", ".c", ".h", ".toml", ".json", ".py", ".sh"]
                .iter()
                .any(|x| lower.ends_with(x));
            (licence_like && !source)
                .then(|| read(&e.path()).ok().map(|t| (name, t)))
                .flatten()
        })
        .collect();
    found.sort();
    found
}

fn read(p: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(p)?;
    Ok(String::from_utf8_lossy(&bytes).replace("\r\n", "\n"))
}

fn render(crates: &[Crate], target: &str) -> String {
    let mut s = String::new();
    let version = env!("CARGO_PKG_VERSION");
    s += &format!("Third-party software in gitvis {version} ({target})\n");
    s += &"=".repeat(60);
    s += "\n\n";
    s += "gitvis is licensed under MIT OR Apache-2.0 (LICENSE-MIT, LICENSE-APACHE).\n";
    s += "Its executable also contains the software listed below: the Rust crates\n";
    s += "it is built from (as resolved in Cargo.lock), the C sources some of them\n";
    s += "vendor (libgit2, zlib) and fonts. The Inter and JetBrains Mono fonts are\n";
    s += "under the SIL Open Font License 1.1; their licences are in this folder.\n";
    s += "Generated by `cargo run -p gitvis-core --example notices`.\n\n";

    s += &format!("Summary ({} crates)\n", crates.len());
    s += &"-".repeat(60);
    s += "\n";
    let width = crates
        .iter()
        .map(|c| c.name.len() + c.version.len() + 1)
        .max()
        .unwrap_or(0);
    for c in crates {
        let id = format!("{} {}", c.name, c.version);
        s += &format!("{id:width$}  {}\n", c.license);
    }

    let mut by_licence: BTreeMap<&str, usize> = BTreeMap::new();
    for c in crates {
        *by_licence.entry(c.license.as_str()).or_default() += 1;
    }
    s += "\nBy licence expression:\n";
    for (l, n) in &by_licence {
        s += &format!("  {n:4}  {l}\n");
    }

    s += "\n\nLicence texts\n";
    s += &"-".repeat(60);
    s += "\n";
    let mut printed: HashMap<&str, String> = HashMap::new();
    for c in crates {
        let id = format!("{} {}", c.name, c.version);
        s += &format!("\n==== {id} ====\nLicence: {}\n", c.license);
        if !c.repository.is_empty() {
            s += &format!("Source: {}\n", c.repository);
        }
        if c.files.is_empty() {
            s += "(The published crate carries no licence file; the standard text of its licence\n";
            s += "is printed in this file for other crates under the same licence.)\n";
        }
        for (name, text) in &c.files {
            match printed.get(text.as_str()) {
                Some(first) => s += &format!("\n--- {name}: same text as {first} above ---\n"),
                None => {
                    s += &format!("\n--- {name} ---\n{}\n", text.trim_end());
                    printed.insert(text.as_str(), format!("{name} of {id}"));
                }
            }
        }
    }
    s
}
