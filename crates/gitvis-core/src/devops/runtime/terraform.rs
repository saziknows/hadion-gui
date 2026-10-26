//! Terraform and OpenTofu roots in a repository.
//!
//! - [`find_roots`] lists the `*.tf` / `*.tofu` files (`git ls-files`,
//!   tracked and untracked but not ignored, or a directory walk outside a
//!   repository), skips `.terraform/`, parses each file loosely (blocks,
//!   attributes and string values; expressions are skipped) and groups them
//!   by directory. Directories another one calls as a local module
//!   (`source = "./networking"`) and directories under `modules/` are
//!   modules, not roots (unless they configure a backend or carry a lock
//!   file).
//! - [`touched`] says which roots a set of changed paths reaches, through
//!   the root's own directory and the local modules it calls.
//! - Command lines for `fmt -check`, `init -backend=false`, `validate
//!   -json`, `plan -detailed-exitcode -out=…`, `show -json` and drift
//!   (`plan -refresh-only`), and parsers for their output. The parsers keep
//!   names, addresses and actions only: plan values can hold secrets.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::devops::proc::{ProcSpec, run_capture};

/// Overrides the terraform / tofu executable (a recording double).
pub const PROGRAM_ENV: &str = "GITVIS_TERRAFORM";
/// Files read per repository at most (a guard for huge monorepos).
const MAX_FILES: usize = 4_000;
/// Bytes read per file at most.
const MAX_FILE_BYTES: u64 = 1_000_000;

/// Which binary runs the commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, Deserialize)]
pub enum Binary {
    #[default]
    Terraform,
    Tofu,
}

impl Binary {
    pub fn program(self) -> String {
        let default = match self {
            Binary::Terraform => "terraform",
            Binary::Tofu => "tofu",
        };
        super::program_override(PROGRAM_ENV, default)
    }

    pub fn name(self) -> &'static str {
        match self {
            Binary::Terraform => "Terraform",
            Binary::Tofu => "OpenTofu",
        }
    }

    /// The command word: `terraform` or `tofu`.
    pub fn word(self) -> &'static str {
        match self {
            Binary::Terraform => "terraform",
            Binary::Tofu => "tofu",
        }
    }

    pub fn tool(self) -> crate::devops::tools::Tool {
        match self {
            Binary::Terraform => crate::devops::tools::Tool::Terraform,
            Binary::Tofu => crate::devops::tools::Tool::Tofu,
        }
    }

    /// Path and version (honours [`PROGRAM_ENV`], which stands in for
    /// Terraform only). Call from a background thread.
    pub fn detect(self) -> Option<crate::devops::tools::Installed> {
        if std::env::var(PROGRAM_ENV).is_ok_and(|v| !v.trim().is_empty()) && self == Binary::Tofu {
            return None;
        }
        super::detect(self.tool(), PROGRAM_ENV, &["version"])
    }

    pub fn install_hint(self) -> &'static str {
        self.tool().install_hint()
    }
}

// ---------------------------------------------------------------------------
// Loose HCL
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Open,
    Close,
    LBracket,
    RBracket,
    LParen,
    RParen,
    Eq,
    Colon,
    Comma,
    Newline,
    Other,
}

/// Skip a `${ … }` / `%{ … }` template inside a string; `i` points after
/// the opening brace. Returns the index after the closing brace.
fn skip_template(b: &[u8], mut i: usize) -> usize {
    let mut depth = 1usize;
    while i < b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            b'"' => {
                i = skip_string(b, i + 1).0;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    i
}

/// Read a quoted string starting after its opening quote. Returns the index
/// after the closing quote and the text (templates kept as written).
fn skip_string(b: &[u8], mut i: usize) -> (usize, String) {
    let mut out = Vec::new();
    while i < b.len() {
        match b[i] {
            b'\\' if i + 1 < b.len() => {
                match b[i + 1] {
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    c => out.push(c),
                }
                i += 2;
            }
            b'"' => return (i + 1, String::from_utf8_lossy(&out).into_owned()),
            b'\n' => break,
            b'$' | b'%' if b.get(i + 1) == Some(&b'{') => {
                let end = skip_template(b, i + 2);
                out.extend_from_slice(&b[i..end.min(b.len())]);
                i = end;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    (i, String::from_utf8_lossy(&out).into_owned())
}

fn tokenize(text: &str) -> Vec<Tok> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        let c = b[i];
        match c {
            b'\n' => {
                out.push(Tok::Newline);
                i += 1;
            }
            b' ' | b'\t' | b'\r' => i += 1,
            b'#' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    if b[i] == b'\n' {
                        out.push(Tok::Newline);
                    }
                    i += 1;
                }
                i += 2;
            }
            b'"' => {
                let (end, s) = skip_string(b, i + 1);
                out.push(Tok::Str(s));
                i = end;
            }
            b'<' if b.get(i + 1) == Some(&b'<') => {
                // Heredoc: <<EOF or <<-EOF, then lines until the marker.
                let mut j = i + 2;
                if b.get(j) == Some(&b'-') {
                    j += 1;
                }
                let mstart = j;
                while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                    j += 1;
                }
                let marker = &text[mstart..j];
                if marker.is_empty() {
                    out.push(Tok::Other);
                    i += 2;
                    continue;
                }
                while j < b.len() && b[j] != b'\n' {
                    j += 1;
                }
                let body_start = (j + 1).min(b.len());
                let mut k = body_start;
                let mut body_end = b.len();
                let mut after = b.len();
                while k < b.len() {
                    let line_end = text[k..].find('\n').map_or(b.len(), |n| k + n);
                    if text[k..line_end].trim() == marker {
                        body_end = k;
                        after = line_end;
                        break;
                    }
                    k = line_end + 1;
                }
                out.push(Tok::Str(
                    text[body_start..body_end.max(body_start)].to_owned(),
                ));
                i = after;
            }
            b'{' => {
                out.push(Tok::Open);
                i += 1;
            }
            b'}' => {
                out.push(Tok::Close);
                i += 1;
            }
            b'[' => {
                out.push(Tok::LBracket);
                i += 1;
            }
            b']' => {
                out.push(Tok::RBracket);
                i += 1;
            }
            b'(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            b')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            b'=' if b.get(i + 1) == Some(&b'=') || b.get(i + 1) == Some(&b'>') => {
                out.push(Tok::Other);
                i += 2;
            }
            b'=' => {
                out.push(Tok::Eq);
                i += 1;
            }
            b':' => {
                out.push(Tok::Colon);
                i += 1;
            }
            b',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < b.len()
                    && (b[i].is_ascii_alphanumeric() || matches!(b[i], b'_' | b'-' | b'.' | b'*'))
                {
                    i += 1;
                }
                out.push(Tok::Ident(text[start..i].to_owned()));
            }
            _ => {
                out.push(Tok::Other);
                i += 1;
            }
        }
    }
    out
}

/// A value, as far as gitvis needs it.
#[derive(Clone, Debug, PartialEq)]
enum Value {
    Str(String),
    /// A bare word: `true`, `false`, `null`, a type name.
    Word(String),
    Object(Vec<Item>),
    Other,
}

#[derive(Clone, Debug, PartialEq)]
enum Item {
    Attr(String, Value),
    Block {
        kind: String,
        labels: Vec<String>,
        body: Vec<Item>,
    },
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    /// Skip an expression up to the end of its line (at nesting depth 0),
    /// or up to a `}` / `,` that closes the enclosing body.
    fn skip_expr(&mut self) {
        let mut depth = 0i32;
        while let Some(t) = self.peek() {
            match t {
                Tok::Open | Tok::LBracket | Tok::LParen => depth += 1,
                Tok::Close | Tok::RBracket | Tok::RParen => {
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                }
                Tok::Newline | Tok::Comma if depth == 0 => return,
                _ => {}
            }
            self.pos += 1;
        }
    }

    fn value(&mut self) -> Value {
        match self.peek() {
            Some(Tok::Open) => {
                self.pos += 1;
                Value::Object(self.body())
            }
            Some(Tok::Str(_)) => {
                let Some(Tok::Str(s)) = self.next() else {
                    unreachable!()
                };
                match self.peek() {
                    None | Some(Tok::Newline | Tok::Close | Tok::Comma) => Value::Str(s),
                    _ => {
                        self.skip_expr();
                        Value::Other
                    }
                }
            }
            Some(Tok::Ident(_)) => {
                let Some(Tok::Ident(w)) = self.next() else {
                    unreachable!()
                };
                match self.peek() {
                    None | Some(Tok::Newline | Tok::Close | Tok::Comma) => Value::Word(w),
                    _ => {
                        self.skip_expr();
                        Value::Other
                    }
                }
            }
            _ => {
                self.skip_expr();
                Value::Other
            }
        }
    }

    /// Items up to the closing `}` (consumed) or the end.
    fn body(&mut self) -> Vec<Item> {
        let mut items = Vec::new();
        loop {
            let Some(t) = self.next() else {
                return items;
            };
            let key = match t {
                Tok::Close => return items,
                Tok::Newline | Tok::Comma => continue,
                Tok::Ident(s) | Tok::Str(s) => s,
                _ => {
                    self.skip_expr();
                    continue;
                }
            };
            match self.peek() {
                Some(Tok::Eq | Tok::Colon) => {
                    self.pos += 1;
                    let v = self.value();
                    items.push(Item::Attr(key, v));
                }
                Some(Tok::Open) => {
                    self.pos += 1;
                    let body = self.body();
                    items.push(Item::Block {
                        kind: key,
                        labels: Vec::new(),
                        body,
                    });
                }
                Some(Tok::Str(_) | Tok::Ident(_)) => {
                    let mut labels = Vec::new();
                    while let Some(Tok::Str(l) | Tok::Ident(l)) = self.peek() {
                        labels.push(l.clone());
                        self.pos += 1;
                    }
                    if self.peek() == Some(&Tok::Open) {
                        self.pos += 1;
                        let body = self.body();
                        items.push(Item::Block {
                            kind: key,
                            labels,
                            body,
                        });
                    } else {
                        self.skip_expr();
                    }
                }
                _ => self.skip_expr(),
            }
        }
    }
}

fn parse_hcl(text: &str) -> Vec<Item> {
    let mut p = Parser {
        toks: tokenize(text),
        pos: 0,
    };
    p.body()
}

fn attr_str<'a>(items: &'a [Item], key: &str) -> Option<&'a str> {
    items.iter().find_map(|i| match i {
        Item::Attr(k, Value::Str(s)) if k == key => Some(s.as_str()),
        _ => None,
    })
}

fn has_attr(items: &[Item], key: &str) -> bool {
    items
        .iter()
        .any(|i| matches!(i, Item::Attr(k, _) if k == key))
}

// ---------------------------------------------------------------------------
// Facts
// ---------------------------------------------------------------------------

/// A provider a root needs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Provider {
    /// Local name: `aws`.
    pub name: String,
    /// `hashicorp/aws` (empty when only a `provider` block names it).
    pub source: String,
    /// The constraint: `~> 5.0`.
    pub version: String,
    /// Locked in `.terraform.lock.hcl`.
    pub locked: Option<String>,
}

/// A `module` call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModuleCall {
    pub name: String,
    pub source: String,
    pub version: String,
    /// The directory a local source (`./x`, `../x`) points at, relative to
    /// the repository.
    pub local_dir: Option<String>,
}

/// A variable declaration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    pub has_default: bool,
    pub sensitive: bool,
    pub description: String,
}

/// One file of a root.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TfFile {
    /// Relative to the repository, forward slashes.
    pub path: String,
    pub bytes: usize,
    pub lines: usize,
}

impl TfFile {
    pub fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

/// A directory with configuration files.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TfDir {
    /// Relative to the repository, forward slashes; `""` for the top.
    pub dir: String,
    pub files: Vec<TfFile>,
    pub providers: Vec<Provider>,
    /// `s3`, `gcs`, `azurerm`, `local`, `remote`, … or `cloud` for a
    /// `cloud {}` block (HCP Terraform).
    pub backend: Option<String>,
    pub required_version: Option<String>,
    pub modules: Vec<ModuleCall>,
    /// `type.name` of every managed resource.
    pub resources: Vec<String>,
    pub data_sources: usize,
    pub variables: Vec<Variable>,
    pub outputs: usize,
    pub locals: usize,
    /// `*.tfvars` files in the directory (names).
    pub tfvars: Vec<String>,
    pub lock_file: bool,
    /// A `.terraform/` directory exists (init ran here).
    pub initialized: bool,
    /// The files mention OpenTofu, or there are `.tofu` files.
    pub mentions_tofu: bool,
}

impl TfDir {
    /// Variables without a default: `plan -input=false` fails unless a
    /// tfvars file (or the environment) sets them.
    pub fn required_variables(&self) -> Vec<&Variable> {
        self.variables.iter().filter(|v| !v.has_default).collect()
    }

    /// `terraform.tfvars` and `*.auto.tfvars` are read automatically.
    pub fn auto_tfvars(&self) -> Vec<&str> {
        self.tfvars
            .iter()
            .filter(|f| {
                *f == "terraform.tfvars"
                    || *f == "terraform.tfvars.json"
                    || f.ends_with(".auto.tfvars")
                    || f.ends_with(".auto.tfvars.json")
            })
            .map(String::as_str)
            .collect()
    }

    pub fn display_dir(&self) -> &str {
        if self.dir.is_empty() { "." } else { &self.dir }
    }
}

/// A root module: where `terraform init / plan` runs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TfRoot {
    pub info: TfDir,
    /// Local module directories it reaches (transitively).
    pub module_dirs: Vec<String>,
}

/// Roots and module directories of a repository.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scan {
    pub roots: Vec<TfRoot>,
    /// Directories found to be modules.
    pub modules: Vec<TfDir>,
    /// Configuration files read.
    pub files_read: usize,
    /// The file limit was hit.
    pub truncated: bool,
}

fn is_config(path: &str) -> bool {
    path.ends_with(".tf") || path.ends_with(".tofu") || path.ends_with(".tf.json")
}

fn is_tfvars(path: &str) -> bool {
    path.ends_with(".tfvars") || path.ends_with(".tfvars.json")
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

fn in_skipped_dir(path: &str) -> bool {
    path.split('/')
        .any(|c| c == ".terraform" || c == ".git" || c == "node_modules")
}

/// `a/b/../c/./d` → `a/c/d`; `None` when it climbs out of the repository.
pub fn normalize(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for c in path.split(['/', '\\']) {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            c => parts.push(c),
        }
    }
    Some(parts.join("/"))
}

/// Facts of one directory from its files' text.
fn analyze_dir(dir: &str, files: &[(String, String)]) -> TfDir {
    let mut d = TfDir {
        dir: dir.to_owned(),
        ..TfDir::default()
    };
    let mut providers: BTreeMap<String, Provider> = BTreeMap::new();
    for (path, text) in files {
        d.files.push(TfFile {
            path: path.clone(),
            bytes: text.len(),
            lines: text.lines().count(),
        });
        if path.ends_with(".tofu") || text.contains("OpenTofu") || text.contains("opentofu") {
            d.mentions_tofu = true;
        }
        if path.ends_with(".json") {
            continue; // JSON syntax: listed, not parsed.
        }
        for item in parse_hcl(text) {
            let Item::Block { kind, labels, body } = item else {
                continue;
            };
            match kind.as_str() {
                "terraform" => {
                    if let Some(v) = attr_str(&body, "required_version") {
                        d.required_version = Some(v.to_owned());
                    }
                    for b in &body {
                        match b {
                            Item::Block {
                                kind,
                                labels,
                                body: inner,
                            } if kind == "required_providers" => {
                                let _ = labels;
                                for p in inner {
                                    match p {
                                        Item::Attr(name, Value::Object(o)) => {
                                            let e = providers.entry(name.clone()).or_default();
                                            e.name = name.clone();
                                            e.source = attr_str(o, "source").unwrap_or("").to_owned();
                                            e.version =
                                                attr_str(o, "version").unwrap_or("").to_owned();
                                        }
                                        Item::Attr(name, Value::Str(v)) => {
                                            let e = providers.entry(name.clone()).or_default();
                                            e.name = name.clone();
                                            e.version = v.clone();
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            Item::Block { kind, labels, .. } if kind == "backend" => {
                                d.backend = labels.first().cloned();
                            }
                            Item::Block { kind, .. } if kind == "cloud" => {
                                d.backend = Some("cloud".into());
                            }
                            _ => {}
                        }
                    }
                }
                "provider" => {
                    if let Some(name) = labels.first() {
                        let e = providers.entry(name.clone()).or_default();
                        e.name = name.clone();
                    }
                }
                "module" => {
                    let name = labels.first().cloned().unwrap_or_default();
                    let source = attr_str(&body, "source").unwrap_or("").to_owned();
                    let local_dir = if source.starts_with("./") || source.starts_with("../") {
                        normalize(&if dir.is_empty() {
                            source.clone()
                        } else {
                            format!("{dir}/{source}")
                        })
                    } else {
                        None
                    };
                    d.modules.push(ModuleCall {
                        name,
                        version: attr_str(&body, "version").unwrap_or("").to_owned(),
                        source,
                        local_dir,
                    });
                }
                "resource" => {
                    if labels.len() >= 2 {
                        d.resources.push(format!("{}.{}", labels[0], labels[1]));
                    }
                }
                "data" => d.data_sources += 1,
                "variable" => d.variables.push(Variable {
                    name: labels.first().cloned().unwrap_or_default(),
                    has_default: has_attr(&body, "default"),
                    sensitive: body.iter().any(
                        |i| matches!(i, Item::Attr(k, Value::Word(w)) if k == "sensitive" && w == "true"),
                    ),
                    description: attr_str(&body, "description").unwrap_or("").to_owned(),
                }),
                "output" => d.outputs += 1,
                "locals" => d.locals += body.len(),
                _ => {}
            }
        }
    }
    d.providers = providers.into_values().collect();
    d.files.sort_by(|a, b| a.path.cmp(&b.path));
    d
}

/// `.terraform.lock.hcl`: `provider "registry.terraform.io/hashicorp/aws" {
/// version = "5.31.0" … }` → `(hashicorp/aws, 5.31.0)`.
pub fn parse_lock(text: &str) -> Vec<(String, String)> {
    parse_hcl(text)
        .into_iter()
        .filter_map(|i| match i {
            Item::Block { kind, labels, body } if kind == "provider" => {
                let addr = labels.first()?;
                let short = addr.split('/').skip(1).collect::<Vec<_>>().join("/");
                Some((
                    if short.is_empty() {
                        addr.clone()
                    } else {
                        short
                    },
                    attr_str(&body, "version").unwrap_or("").to_owned(),
                ))
            }
            _ => None,
        })
        .collect()
}

/// Group configuration files into directories and tell roots from
/// modules. `files` are `(relative path, text)`; `exists` answers whether a
/// relative path exists (for `.terraform/`).
pub fn analyze(files: Vec<(String, String)>, exists: impl Fn(&str) -> bool) -> Scan {
    let mut by_dir: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut tfvars: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut locks: BTreeMap<String, String> = BTreeMap::new();
    let files_read = files.len();
    for (path, text) in files {
        if in_skipped_dir(&path) {
            continue;
        }
        let dir = dir_of(&path).to_owned();
        let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
        if name == ".terraform.lock.hcl" {
            locks.insert(dir, text);
        } else if is_tfvars(&path) {
            tfvars.entry(dir).or_default().push(name);
        } else if is_config(&path) {
            by_dir.entry(dir).or_default().push((path, text));
        }
    }
    let mut dirs: Vec<TfDir> = by_dir
        .iter()
        .map(|(dir, files)| {
            let mut d = analyze_dir(dir, files);
            d.tfvars = tfvars.get(dir).cloned().unwrap_or_default();
            d.tfvars.sort();
            if let Some(lock) = locks.get(dir) {
                d.lock_file = true;
                let locked = parse_lock(lock);
                for p in &mut d.providers {
                    let want = if p.source.is_empty() {
                        format!("hashicorp/{}", p.name)
                    } else {
                        p.source.to_ascii_lowercase()
                    };
                    p.locked = locked
                        .iter()
                        .find(|(a, _)| a.eq_ignore_ascii_case(&want))
                        .map(|(_, v)| v.clone());
                }
            }
            let dot_tf = if dir.is_empty() {
                ".terraform".to_owned()
            } else {
                format!("{dir}/.terraform")
            };
            d.initialized = exists(&dot_tf);
            d
        })
        .collect();
    // Modules: called by another directory, or under `modules/`.
    let called: BTreeSet<String> = dirs
        .iter()
        .flat_map(|d| d.modules.iter().filter_map(|m| m.local_dir.clone()))
        .collect();
    let mut roots = Vec::new();
    let mut modules = Vec::new();
    for d in dirs.drain(..) {
        let under_modules = d.dir.split('/').any(|c| c == "modules");
        let pinned_root = d.backend.is_some() || d.lock_file;
        if called.contains(&d.dir) || (under_modules && !pinned_root) {
            modules.push(d);
        } else {
            roots.push(d);
        }
    }
    let module_map: BTreeMap<String, Vec<String>> = modules
        .iter()
        .chain(roots.iter())
        .map(|d| {
            (
                d.dir.clone(),
                d.modules
                    .iter()
                    .filter_map(|m| m.local_dir.clone())
                    .collect(),
            )
        })
        .collect();
    let roots = roots
        .into_iter()
        .map(|info| {
            // Transitive local modules.
            let mut seen: BTreeSet<String> = BTreeSet::new();
            let mut stack: Vec<String> = info
                .modules
                .iter()
                .filter_map(|m| m.local_dir.clone())
                .collect();
            while let Some(m) = stack.pop() {
                if m == info.dir || !seen.insert(m.clone()) {
                    continue;
                }
                if let Some(next) = module_map.get(&m) {
                    stack.extend(next.iter().cloned());
                }
            }
            TfRoot {
                info,
                module_dirs: seen.into_iter().collect(),
            }
        })
        .collect();
    Scan {
        roots,
        modules,
        files_read,
        truncated: false,
    }
}

/// Relative paths of the configuration, variable and lock files: `git
/// ls-files` (tracked and untracked, not ignored), or a directory walk when
/// `dir` is not a repository.
pub fn list_files(dir: &Path) -> Result<Vec<String>, String> {
    let spec = ProcSpec::new("git", dir).args([
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
        "--",
        "*.tf",
        "*.tofu",
        "*.tf.json",
        "*.tfvars",
        "*.tfvars.json",
        "*.terraform.lock.hcl",
    ]);
    match run_capture(&spec, Some(Duration::from_secs(60))) {
        Ok(out) if out.success() => {
            let mut v: Vec<String> = out
                .stdout
                .split(|b| *b == 0)
                .filter(|p| !p.is_empty())
                .map(|p| String::from_utf8_lossy(p).replace('\\', "/"))
                .filter(|p| !in_skipped_dir(p))
                .collect();
            v.sort();
            v.dedup();
            Ok(v)
        }
        _ => Ok(walk(dir)),
    }
}

fn walk(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if depth < 16
                    && !matches!(
                        name.as_str(),
                        ".git" | ".terraform" | "node_modules" | "target"
                    )
                {
                    stack.push((e.path(), depth + 1));
                }
            } else if is_config(&name) || is_tfvars(&name) || name == ".terraform.lock.hcl" {
                if let Ok(rel) = e.path().strip_prefix(root) {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
            if out.len() >= MAX_FILES {
                return out;
            }
        }
    }
    out.sort();
    out
}

/// Find and analyse the roots of `dir`.
pub fn find_roots(dir: &Path) -> Result<Scan, String> {
    let paths = list_files(dir)?;
    let truncated = paths.len() > MAX_FILES;
    let mut files = Vec::new();
    for p in paths.into_iter().take(MAX_FILES) {
        let full = dir.join(&p);
        let Ok(meta) = std::fs::metadata(&full) else {
            continue; // deleted in the working tree
        };
        if meta.len() > MAX_FILE_BYTES {
            files.push((p, String::new()));
            continue;
        }
        let text = std::fs::read(&full)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        files.push((p, text));
    }
    let mut scan = analyze(files, |rel| dir.join(rel).exists());
    scan.truncated = truncated;
    Ok(scan)
}

/// Which roots `changed` (relative paths) reaches: files in the root's
/// directory (not in a nested root; for a root at the top, only
/// configuration files) and in the local modules it calls. Returns, per
/// root index, the changed paths.
pub fn touched(roots: &[TfRoot], changed: &[String]) -> Vec<(usize, Vec<String>)> {
    let dirs: Vec<&str> = roots.iter().map(|r| r.info.dir.as_str()).collect();
    let under =
        |path: &str, dir: &str| -> bool { dir.is_empty() || path.starts_with(&format!("{dir}/")) };
    let mut out = Vec::new();
    for (i, r) in roots.iter().enumerate() {
        let dir = r.info.dir.as_str();
        let mut hits: Vec<String> = Vec::new();
        for path in changed {
            let path = path.replace('\\', "/");
            let in_module = r.module_dirs.iter().any(|m| under(&path, m));
            let in_root = under(&path, dir)
                && !dirs
                    .iter()
                    .any(|d| *d != dir && d.len() > dir.len() && under(d, dir) && under(&path, d))
                && (!dir.is_empty() || is_config(&path) || is_tfvars(&path));
            if (in_module || in_root) && !hits.contains(&path) {
                hits.push(path);
            }
        }
        if !hits.is_empty() {
            out.push((i, hits));
        }
    }
    out
}

/// `git diff --name-only @{u}...HEAD`: what the commits not pushed yet
/// change. `Ok(None)` when there is no upstream.
pub fn unpushed_changes(dir: &Path) -> Result<Vec<String>, String> {
    let spec = ProcSpec::new("git", dir).args(["diff", "--name-only", "-z", "@{u}...HEAD"]);
    let out = run_capture(&spec, Some(Duration::from_secs(60))).map_err(|e| e.to_string())?;
    if !out.success() {
        return Err(out.error_text());
    }
    Ok(out
        .stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect())
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// What can be run on a root.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    Fmt,
    Init,
    Validate,
    Plan,
    Drift,
}

impl Command {
    pub const ALL: [Command; 5] = [
        Command::Fmt,
        Command::Init,
        Command::Validate,
        Command::Plan,
        Command::Drift,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Command::Fmt => "Format check",
            Command::Init => "Init",
            Command::Validate => "Validate",
            Command::Plan => "Plan",
            Command::Drift => "Drift check",
        }
    }

    /// Talks to the network (registry, providers' APIs, state backend).
    pub fn network(self) -> bool {
        matches!(self, Command::Init | Command::Plan | Command::Drift)
    }

    /// The arguments (after the program). `plan_out` is the plan file for
    /// [`Command::Plan`].
    pub fn args(self, plan_out: Option<&Path>) -> Vec<String> {
        let v: Vec<&str> = match self {
            Command::Fmt => vec!["fmt", "-check", "-recursive", "-diff", "-no-color"],
            Command::Init => vec!["init", "-backend=false", "-input=false", "-no-color"],
            Command::Validate => vec!["validate", "-json", "-no-color"],
            Command::Plan => vec![
                "plan",
                "-input=false",
                "-no-color",
                "-lock=false",
                "-detailed-exitcode",
            ],
            Command::Drift => vec![
                "plan",
                "-refresh-only",
                "-input=false",
                "-no-color",
                "-lock=false",
                "-detailed-exitcode",
            ],
        };
        let mut a: Vec<String> = v.into_iter().map(str::to_owned).collect();
        if self == Command::Plan
            && let Some(p) = plan_out
        {
            a.push(format!("-out={}", p.display()));
        }
        a
    }

    /// What it does, in a sentence (shown next to the button).
    pub fn explain(self) -> &'static str {
        match self {
            Command::Fmt => {
                "Lists files that are not in canonical format and shows the diff. Changes nothing."
            }
            Command::Init => {
                "Downloads the providers and modules into .terraform/ (network). The backend is skipped, so no state is read or written."
            }
            Command::Validate => {
                "Checks the configuration for errors, with file and line. Needs init first (providers and modules)."
            }
            Command::Plan => {
                "Refreshes real infrastructure and shows what would change (network, provider credentials and backend access). No lock is taken and nothing is applied."
            }
            Command::Drift => {
                "Refresh-only plan: reports resources that changed outside Terraform since the last apply (network, credentials, backend)."
            }
        }
    }
}

/// The process for a command in a root directory.
pub fn spec(binary: Binary, root_dir: &Path, cmd: Command, plan_out: Option<&Path>) -> ProcSpec {
    ProcSpec::new(binary.program(), root_dir).args(cmd.args(plan_out))
}

/// `show -json PLANFILE`.
pub fn show_args(plan: &Path) -> Vec<String> {
    vec![
        "show".into(),
        "-json".into(),
        "-no-color".into(),
        plan.display().to_string(),
    ]
}

/// A fresh plan file path in the temp directory for a root.
pub fn plan_file(root_dir: &str) -> PathBuf {
    let slug: String = root_dir
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "gitvis-plan-{}-{nanos}.tfplan",
        if slug.is_empty() { "root".into() } else { slug }
    ))
}

/// Run `show -json` on a plan file, parse it, delete the file (plans can
/// hold secrets). Only names and actions are kept.
pub fn show_plan(
    binary: Binary,
    root_dir: &Path,
    plan: &Path,
    commands: &mut Vec<String>,
) -> Result<PlanJson, String> {
    let args = show_args(plan);
    commands.push(format!("{} {}", binary.word(), args.join(" ")));
    let spec = ProcSpec::new(binary.program(), root_dir).args(args);
    let out = run_capture(&spec, Some(Duration::from_secs(120)));
    let _ = std::fs::remove_file(plan);
    let out = out.map_err(|e| e.to_string())?;
    if !out.success() {
        return Err(out.error_text());
    }
    parse_show_json(&out.stdout)
}

// ---------------------------------------------------------------------------
// Output parsers
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct RawPos {
    line: u32,
    column: u32,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct RawRange {
    filename: String,
    start: RawPos,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct RawSnippet {
    context: Option<String>,
    code: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct RawDiag {
    severity: String,
    summary: String,
    detail: String,
    range: Option<RawRange>,
    snippet: Option<RawSnippet>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct RawValidate {
    format_version: String,
    valid: bool,
    error_count: usize,
    warning_count: usize,
    diagnostics: Vec<RawDiag>,
}

/// One diagnostic of `validate -json`, or an `Error:` block of plain output.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diagnostic {
    /// `error` or `warning`.
    pub severity: String,
    pub summary: String,
    pub detail: String,
    /// Relative to the root directory.
    pub file: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    /// The block it is in: `resource "aws_instance" "web"`.
    pub context: Option<String>,
    /// The source line.
    pub code: Option<String>,
}

impl Diagnostic {
    pub fn is_error(&self) -> bool {
        self.severity == "error"
    }

    /// `main.tf:12:3`.
    pub fn location(&self) -> Option<String> {
        let f = self.file.as_ref()?;
        Some(match (self.line, self.column) {
            (Some(l), Some(c)) => format!("{f}:{l}:{c}"),
            (Some(l), None) => format!("{f}:{l}"),
            _ => f.clone(),
        })
    }
}

/// The answer of `validate -json`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Validation {
    pub valid: bool,
    pub errors: usize,
    pub warnings: usize,
    pub diagnostics: Vec<Diagnostic>,
}

/// Parse `validate -json` output (the JSON object may follow other lines).
pub fn parse_validate(text: &str) -> Result<Validation, String> {
    let start = text
        .find('{')
        .ok_or_else(|| "validate printed no JSON".to_owned())?;
    let end = text.rfind('}').map_or(text.len(), |e| e + 1);
    let raw: RawValidate =
        serde_json::from_str(&text[start..end]).map_err(|e| format!("validate -json: {e}"))?;
    if raw.format_version.is_empty() && raw.diagnostics.is_empty() && !raw.valid {
        return Err("validate -json: unexpected answer".into());
    }
    Ok(Validation {
        valid: raw.valid,
        errors: raw.error_count,
        warnings: raw.warning_count,
        diagnostics: raw
            .diagnostics
            .into_iter()
            .map(|d| Diagnostic {
                severity: d.severity,
                summary: d.summary,
                detail: d.detail,
                file: d
                    .range
                    .as_ref()
                    .map(|r| r.filename.clone())
                    .filter(|f| !f.is_empty()),
                line: d.range.as_ref().map(|r| r.start.line).filter(|l| *l > 0),
                column: d.range.as_ref().map(|r| r.start.column).filter(|c| *c > 0),
                context: d.snippet.as_ref().and_then(|s| s.context.clone()),
                code: d
                    .snippet
                    .as_ref()
                    .map(|s| s.code.trim_end().to_owned())
                    .filter(|c| !c.is_empty()),
            })
            .collect(),
    })
}

/// What a plan does to one resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PlanAction {
    Delete,
    Replace,
    Update,
    Create,
    Import,
    Read,
    Forget,
    Move,
    /// Changed outside Terraform (drift).
    Drifted,
    /// Deleted outside Terraform (drift).
    DriftDeleted,
    NoOp,
}

impl PlanAction {
    pub fn label(self) -> &'static str {
        match self {
            PlanAction::Create => "Create",
            PlanAction::Update => "Update in place",
            PlanAction::Delete => "Destroy",
            PlanAction::Replace => "Replace",
            PlanAction::Read => "Read",
            PlanAction::Import => "Import",
            PlanAction::Forget => "Forget",
            PlanAction::Move => "Move",
            PlanAction::Drifted => "Changed outside",
            PlanAction::DriftDeleted => "Deleted outside",
            PlanAction::NoOp => "No change",
        }
    }

    /// `+`, `~`, `-`, `-/+`, `<=`.
    pub fn symbol(self) -> &'static str {
        match self {
            PlanAction::Create => "+",
            PlanAction::Update | PlanAction::Drifted => "~",
            PlanAction::Delete | PlanAction::DriftDeleted => "-",
            PlanAction::Replace => "-/+",
            PlanAction::Read => "<=",
            PlanAction::Import => "←",
            PlanAction::Forget => "⊘",
            PlanAction::Move => "→",
            PlanAction::NoOp => "·",
        }
    }

    /// From a `show -json` `actions` array.
    pub fn from_actions(actions: &[String], importing: bool) -> Self {
        let a: Vec<&str> = actions.iter().map(String::as_str).collect();
        match a.as_slice() {
            ["create"] => PlanAction::Create,
            ["update"] => PlanAction::Update,
            ["delete"] => PlanAction::Delete,
            ["delete", "create"] | ["create", "delete"] => PlanAction::Replace,
            ["read"] => PlanAction::Read,
            ["forget"] => PlanAction::Forget,
            ["no-op"] if importing => PlanAction::Import,
            _ if importing => PlanAction::Import,
            _ => PlanAction::NoOp,
        }
    }
}

/// One resource line of a plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceChange {
    pub address: String,
    pub action: PlanAction,
    /// `(because … is not in configuration)` and similar.
    pub reason: Option<String>,
    pub resource_type: String,
    pub provider: String,
}

impl ResourceChange {
    fn new(address: &str, action: PlanAction) -> Self {
        Self {
            address: address.to_owned(),
            action,
            reason: None,
            resource_type: resource_type(address),
            provider: String::new(),
        }
    }

    /// `module.net.aws_vpc.main` → `module.net`.
    pub fn module(&self) -> Option<&str> {
        module_of(&self.address)
    }
}

fn module_of(address: &str) -> Option<&str> {
    let mut end = None;
    let mut parts = address.split('.').peekable();
    let mut pos = 0usize;
    while let Some(p) = parts.next() {
        if p.starts_with("module") && parts.peek().is_some() {
            let name = parts.next().unwrap_or("");
            pos += p.len() + 1 + name.len() + 1;
            end = Some(pos - 1);
        } else {
            break;
        }
    }
    end.map(|e| &address[..e.min(address.len())])
}

/// `module.x.aws_instance.web["a"]` → `aws_instance`.
pub fn resource_type(address: &str) -> String {
    let rest = module_of(address).map_or(address, |m| &address[(m.len() + 1).min(address.len())..]);
    let rest = rest.strip_prefix("data.").unwrap_or(rest);
    rest.split('.').next().unwrap_or("").to_owned()
}

/// "Plan: 1 to import, 2 to add, 1 to change, 0 to destroy."
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub add: usize,
    pub change: usize,
    pub destroy: usize,
    pub import: usize,
    pub forget: usize,
}

impl Counts {
    pub fn total(&self) -> usize {
        self.add + self.change + self.destroy + self.import + self.forget
    }
}

/// What the plain-text output of `plan` says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlanText {
    pub counts: Option<Counts>,
    /// "No changes. Your infrastructure matches the configuration."
    pub no_changes: bool,
    pub resources: Vec<ResourceChange>,
    /// "Objects have changed outside of Terraform" (and refresh-only).
    pub drift: Vec<ResourceChange>,
    /// Output names under "Changes to Outputs:", with `+` / `~` / `-`.
    pub outputs: Vec<(String, String)>,
    pub errors: Vec<Diagnostic>,
    pub warnings: Vec<Diagnostic>,
}

impl PlanText {
    /// Counts from the summary line, else from the resource lines.
    pub fn counts(&self) -> Counts {
        if let Some(c) = self.counts {
            return c;
        }
        let mut c = Counts::default();
        for r in &self.resources {
            match r.action {
                PlanAction::Create => c.add += 1,
                PlanAction::Update => c.change += 1,
                PlanAction::Delete => c.destroy += 1,
                PlanAction::Replace => {
                    c.add += 1;
                    c.destroy += 1;
                }
                PlanAction::Import => c.import += 1,
                PlanAction::Forget => c.forget += 1,
                _ => {}
            }
        }
        c
    }
}

/// `# aws_instance.web will be created` → the address and action.
fn resource_line(line: &str) -> Option<ResourceChange> {
    let rest = line.trim().strip_prefix("# ")?;
    const PHRASES: [(&str, PlanAction); 14] = [
        (" will be created", PlanAction::Create),
        (" will be updated in-place", PlanAction::Update),
        (" will be destroyed", PlanAction::Delete),
        (" must be replaced", PlanAction::Replace),
        (" will be replaced, as requested", PlanAction::Replace),
        (
            " will be replaced due to changes in replace_triggered_by",
            PlanAction::Replace,
        ),
        (" will be read during apply", PlanAction::Read),
        (" will be imported", PlanAction::Import),
        (
            " will no longer be managed by Terraform",
            PlanAction::Forget,
        ),
        (" will no longer be managed by OpenTofu", PlanAction::Forget),
        (" will be forgotten", PlanAction::Forget),
        (" has moved to ", PlanAction::Move),
        (" has been deleted", PlanAction::DriftDeleted),
        (" has changed", PlanAction::Drifted),
    ];
    for (phrase, action) in PHRASES {
        if let Some(i) = rest.find(phrase) {
            let addr = rest[..i].trim();
            if addr.is_empty() || addr.contains(' ') && !addr.contains('"') {
                return None;
            }
            let mut rc = ResourceChange::new(addr, action);
            if action == PlanAction::Move {
                rc.reason = Some(format!("moved to {}", rest[i + phrase.len()..].trim()));
            }
            return Some(rc);
        }
    }
    None
}

/// Strip the box drawing of newer diagnostics (`│ `, `╷`, `╵`).
fn unbox(line: &str) -> &str {
    let t = line.trim_end();
    let t = t.strip_prefix('│').unwrap_or(t);
    let t = t.strip_prefix(' ').unwrap_or(t);
    t
}

/// `Error: …` / `Warning: …` blocks with their detail lines.
pub fn parse_diagnostics(text: &str) -> (Vec<Diagnostic>, Vec<Diagnostic>) {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let l = unbox(lines[i]).trim_start();
        let (sev, summary) = if let Some(s) = l.strip_prefix("Error: ") {
            ("error", s)
        } else if let Some(s) = l.strip_prefix("Warning: ") {
            ("warning", s)
        } else {
            i += 1;
            continue;
        };
        let mut detail: Vec<String> = Vec::new();
        let mut file = None;
        let mut line_no = None;
        let mut j = i + 1;
        while j < lines.len() {
            let raw = lines[j].trim_end();
            if raw.starts_with('╵') || (raw.is_empty() && !lines[j].starts_with('│')) {
                break;
            }
            let d = unbox(raw);
            if d.starts_with("Error: ") || d.starts_with("Warning: ") {
                break;
            }
            // `  on main.tf line 12, in resource "x" "y":`
            if let Some(on) = d.trim_start().strip_prefix("on ")
                && let Some((f, rest)) = on.split_once(" line ")
            {
                file = Some(f.trim().to_owned());
                line_no = rest
                    .split(|c: char| !c.is_ascii_digit())
                    .next()
                    .and_then(|n| n.parse().ok());
            }
            detail.push(d.to_owned());
            j += 1;
        }
        while detail.first().is_some_and(|d| d.trim().is_empty()) {
            detail.remove(0);
        }
        while detail.last().is_some_and(|d| d.trim().is_empty()) {
            detail.pop();
        }
        let diag = Diagnostic {
            severity: sev.into(),
            summary: summary.trim().to_owned(),
            detail: detail.join("\n"),
            file,
            line: line_no,
            ..Diagnostic::default()
        };
        if sev == "error" {
            errors.push(diag);
        } else {
            warnings.push(diag);
        }
        i = j;
    }
    (errors, warnings)
}

/// Parse the plain-text output of `plan` (or `plan -refresh-only`).
pub fn parse_plan_text(text: &str) -> PlanText {
    let mut p = PlanText::default();
    let summary = regex::Regex::new(r"(\d+) to (add|change|destroy|import|forget)").expect("regex");
    let mut in_drift = false;
    let mut in_outputs = false;
    let mut last: Option<(bool, usize)> = None;
    for raw in text.lines() {
        let line = raw.trim_end();
        let t = line.trim();
        if t.starts_with("Objects have changed outside of") {
            in_drift = true;
            in_outputs = false;
            continue;
        }
        if t.starts_with("Terraform used the selected providers")
            || t.starts_with("OpenTofu used the selected providers")
            || t.starts_with("Terraform will perform the following actions")
            || t.starts_with("OpenTofu will perform the following actions")
        {
            in_drift = false;
            in_outputs = false;
            continue;
        }
        if t.starts_with("Changes to Outputs:") {
            in_outputs = true;
            continue;
        }
        if t.starts_with("No changes.") {
            p.no_changes = true;
            continue;
        }
        if let Some(rest) = t.strip_prefix("Plan:") {
            let mut c = Counts::default();
            for cap in summary.captures_iter(rest) {
                let n: usize = cap[1].parse().unwrap_or(0);
                match &cap[2] {
                    "add" => c.add = n,
                    "change" => c.change = n,
                    "destroy" => c.destroy = n,
                    "import" => c.import = n,
                    "forget" => c.forget = n,
                    _ => {}
                }
            }
            p.counts = Some(c);
            in_outputs = false;
            continue;
        }
        if let Some(rc) = resource_line(t) {
            in_outputs = false;
            let drift =
                in_drift || matches!(rc.action, PlanAction::Drifted | PlanAction::DriftDeleted);
            if drift {
                p.drift.push(rc);
                last = Some((true, p.drift.len() - 1));
            } else {
                p.resources.push(rc);
                last = Some((false, p.resources.len() - 1));
            }
            continue;
        }
        if let Some(reason) = t.strip_prefix("# (").and_then(|r| r.strip_suffix(')')) {
            if let Some((drift, i)) = last {
                let list = if drift {
                    &mut p.drift
                } else {
                    &mut p.resources
                };
                if let Some(rc) = list.get_mut(i)
                    && rc.reason.is_none()
                {
                    rc.reason = Some(reason.to_owned());
                }
            }
            continue;
        }
        if in_outputs {
            // `  + vpc_id = (known after apply)`
            let mut parts = t.splitn(3, ' ');
            if let (Some(sym), Some(name)) = (parts.next(), parts.next())
                && matches!(sym, "+" | "~" | "-")
                && !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                p.outputs.push((name.to_owned(), sym.to_owned()));
            }
        }
    }
    let (errors, warnings) = parse_diagnostics(text);
    p.errors = errors;
    p.warnings = warnings;
    p
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct RawChange {
    actions: Vec<String>,
    importing: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct RawResourceChange {
    address: String,
    mode: String,
    #[serde(rename = "type")]
    kind: String,
    provider_name: String,
    change: RawChange,
    action_reason: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct RawOutputChange {
    actions: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct RawPlan {
    format_version: String,
    terraform_version: String,
    resource_changes: Vec<RawResourceChange>,
    resource_drift: Vec<RawResourceChange>,
    output_changes: BTreeMap<String, RawOutputChange>,
    errored: bool,
    applyable: Option<bool>,
}

/// The structured plan from `show -json`: addresses and actions only.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlanJson {
    pub format_version: String,
    pub terraform_version: String,
    /// Resources that change (no-ops left out).
    pub resources: Vec<ResourceChange>,
    pub unchanged: usize,
    pub drift: Vec<ResourceChange>,
    /// `(output, action)` for outputs that change.
    pub outputs: Vec<(String, PlanAction)>,
    pub errored: bool,
    pub applyable: Option<bool>,
}

impl PlanJson {
    pub fn counts(&self) -> Counts {
        let mut c = Counts::default();
        for r in &self.resources {
            match r.action {
                PlanAction::Create => c.add += 1,
                PlanAction::Update => c.change += 1,
                PlanAction::Delete => c.destroy += 1,
                PlanAction::Replace => {
                    c.add += 1;
                    c.destroy += 1;
                }
                PlanAction::Import => c.import += 1,
                PlanAction::Forget => c.forget += 1,
                _ => {}
            }
        }
        c
    }
}

fn reason_text(r: &str) -> String {
    match r {
        "replace_because_cannot_update" => "an argument that forces replacement changed".into(),
        "replace_because_tainted" => "it is tainted".into(),
        "replace_by_request" => "replacement was requested (-replace)".into(),
        "replace_by_triggers" => "replace_triggered_by fired".into(),
        "delete_because_no_resource_config" => "it is not in the configuration".into(),
        "delete_because_no_module" => "its module is not in the configuration".into(),
        "delete_because_wrong_repetition" => "count / for_each changed".into(),
        "delete_because_count_index" => "its count index is gone".into(),
        "delete_because_each_key" => "its for_each key is gone".into(),
        "read_because_config_unknown" => {
            "its configuration depends on values known only at apply".into()
        }
        "read_because_dependency_pending" => "it depends on a pending change".into(),
        other => other.replace('_', " "),
    }
}

/// Parse `show -json PLANFILE`. Attribute values are skipped.
pub fn parse_show_json(bytes: &[u8]) -> Result<PlanJson, String> {
    let raw: RawPlan = serde_json::from_slice(bytes).map_err(|e| format!("show -json: {e}"))?;
    let conv = |r: &RawResourceChange, drift: bool| {
        let importing = r.change.importing.is_some();
        let mut action = PlanAction::from_actions(&r.change.actions, importing);
        if drift {
            action = if action == PlanAction::Delete {
                PlanAction::DriftDeleted
            } else {
                PlanAction::Drifted
            };
        }
        let address = if r.mode == "data" && !r.address.contains("data.") {
            format!("data.{}", r.address)
        } else {
            r.address.clone()
        };
        ResourceChange {
            resource_type: if r.kind.is_empty() {
                resource_type(&address)
            } else {
                r.kind.clone()
            },
            provider: r
                .provider_name
                .rsplit('/')
                .take(2)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("/"),
            reason: r.action_reason.as_deref().map(reason_text),
            action,
            address,
        }
    };
    let all: Vec<ResourceChange> = raw
        .resource_changes
        .iter()
        .map(|r| conv(r, false))
        .collect();
    let unchanged = all.iter().filter(|r| r.action == PlanAction::NoOp).count();
    Ok(PlanJson {
        format_version: raw.format_version,
        terraform_version: raw.terraform_version,
        resources: all
            .into_iter()
            .filter(|r| r.action != PlanAction::NoOp)
            .collect(),
        unchanged,
        drift: raw.resource_drift.iter().map(|r| conv(r, true)).collect(),
        outputs: raw
            .output_changes
            .iter()
            .map(|(k, v)| (k.clone(), PlanAction::from_actions(&v.actions, false)))
            .filter(|(_, a)| *a != PlanAction::NoOp)
            .collect(),
        errored: raw.errored,
        applyable: raw.applyable,
    })
}

/// How `plan -detailed-exitcode` ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanOutcome {
    /// Exit 0.
    NoChanges,
    /// Exit 2.
    Changes,
    /// Exit 1 (or anything else).
    Failed,
    /// Killed or stopped.
    Stopped,
}

pub fn plan_outcome(code: Option<i32>) -> PlanOutcome {
    match code {
        Some(0) => PlanOutcome::NoChanges,
        Some(2) => PlanOutcome::Changes,
        Some(_) => PlanOutcome::Failed,
        None => PlanOutcome::Stopped,
    }
}

/// How a drift check ended: exit 2 of `plan -refresh-only
/// -detailed-exitcode` means something changed outside Terraform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriftOutcome {
    InSync,
    Drift,
    Failed,
    Stopped,
}

pub fn drift_outcome(code: Option<i32>) -> DriftOutcome {
    match plan_outcome(code) {
        PlanOutcome::NoChanges => DriftOutcome::InSync,
        PlanOutcome::Changes => DriftOutcome::Drift,
        PlanOutcome::Failed => DriftOutcome::Failed,
        PlanOutcome::Stopped => DriftOutcome::Stopped,
    }
}

/// `fmt -check -diff` output: the files that need formatting (each listed
/// on its own line before its diff).
pub fn parse_fmt(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for l in text.lines() {
        let t = l.trim();
        if t.is_empty()
            || t.starts_with("---")
            || t.starts_with("+++")
            || t.starts_with("@@")
            || t.starts_with("diff ")
            || l.starts_with(['+', '-', ' '])
        {
            continue;
        }
        if is_config(t) || is_tfvars(t) {
            if !out.contains(&t.to_owned()) {
                out.push(t.to_owned());
            }
        }
    }
    out
}

/// A likely cause for a failed command, from its output.
pub fn likely_cause(text: &str, binary: Binary) -> Option<String> {
    let l = text.to_ascii_lowercase();
    let w = binary.word();
    let any = |pats: &[&str]| pats.iter().any(|p| l.contains(p));
    if any(&[
        "no valid credential sources",
        "nocredentialproviders",
        "unable to locate credentials",
        "could not find default credentials",
        "failed to refresh cached credentials",
        "expiredtoken",
        "the security token included in the request is invalid",
        "error building account",
        "please run 'az login'",
        "invalidclienttokenid",
    ]) {
        return Some(
            "The provider found no cloud credentials on this machine (or they expired). Set them up the way the provider expects (AWS_PROFILE / aws sso login, gcloud auth application-default login, az login) and run it again.".into(),
        );
    }
    if any(&[
        "backend initialization required",
        "backend configuration changed",
        "initial configuration of the requested backend",
    ]) {
        return Some(format!(
            "The state backend is not initialised. gitvis's Init uses -backend=false (it never reads state); a real plan needs `{w} init` with the backend configuration, which needs access to the state storage."
        ));
    }
    if any(&[
        "required plugins are not installed",
        "missing required provider",
        "provider isn't available",
        "install it automatically",
        "inconsistent dependency lock file",
        "please run \"terraform init\"",
        "please run \"tofu init\"",
        "run \"terraform init\"",
        "run \"tofu init\"",
    ]) {
        return Some(
            "Providers are not installed in .terraform/ yet: run Init first (it downloads them)."
                .into(),
        );
    }
    if any(&[
        "module not installed",
        "module source has changed",
        "module is not yet installed",
    ]) {
        return Some("Modules are not installed yet: run Init first.".into());
    }
    if any(&["no value for required variable"]) {
        return Some("A variable without a default has no value. Plans run with -input=false: set it in terraform.tfvars, a *.auto.tfvars file or a TF_VAR_name environment variable.".into());
    }
    if any(&[
        "nosuchbucket",
        "bucket does not exist",
        "replace_with",
        "failed to get existing workspaces",
    ]) {
        return Some("The backend points at storage that does not exist or is a placeholder. Check the backend block (and -backend-config values).".into());
    }
    if any(&[
        "unsupported terraform core version",
        "unsupported opentofu core version",
    ]) {
        return Some(format!(
            "The installed {} does not satisfy required_version. Install a matching version.",
            binary.name()
        ));
    }
    if any(&[
        "failed to query available provider packages",
        "could not connect to registry",
        "failed to install provider",
        "no such host",
        "i/o timeout",
    ]) {
        return Some(
            "The provider registry could not be reached: check the network or proxy.".into(),
        );
    }
    if any(&["error acquiring the state lock"]) {
        return Some("Someone else holds the state lock.".into());
    }
    if any(&[
        "unsupported argument",
        "unsupported block type",
        "missing required argument",
        "reference to undeclared",
        "invalid reference",
        "argument or block definition required",
    ]) {
        return Some(
            "The configuration has errors: Validate lists each one with its file and line.".into(),
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: &str = r#"
# Root module: wires networking/eks together.
module "networking" {
  source = "./networking"

  aws_region  = var.aws_region
  environment = var.environment
}

module "eks" {
  source  = "terraform-aws-modules/eks/aws"
  version = "~> 20.31"
  cluster_name = "shop-${var.environment}"
  tags = {
    Name = "x"   # a comment with "quotes" and { braces }
  }
}

module "db" {
  source = "../shared/db"
}

variable "preview_domain" {
  description = "e.g. preview.shop-domain.io"
  type        = string
}

variable "db_password" {
  type      = string
  sensitive = true
  default   = null
}

variable "aws_region" {
  type    = string
  default = "us-east-1"
}

resource "aws_s3_bucket" "logs" {
  bucket = "logs-${var.environment}"
  policy = <<EOF
{ "Version": "2012-10-17", "Statement": [ { "Effect": "Allow" } ] }
EOF
}

data "aws_caller_identity" "me" {}

output "bucket" {
  value = aws_s3_bucket.logs.id
}

locals {
  a = 1
  b = { c = 2 }
}
"#;

    const BACKEND: &str = r#"
terraform {
  required_version = ">= 1.7.0" # OpenTofu

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.0"
    }
    random = "~> 3.5"
  }

  backend "s3" {
    bucket = "REPLACE_WITH_STATE_BUCKET"
    key    = "shop/bootstrap/terraform.tfstate"
  }
}

provider "aws" {
  region = var.aws_region
}

provider "kubernetes" {
  host = module.eks.cluster_endpoint
}
"#;

    const LOCK: &str = r#"
# This file is maintained automatically by "terraform init".
provider "registry.terraform.io/hashicorp/aws" {
  version     = "5.31.0"
  constraints = "~> 5.0"
  hashes = [
    "h1:abc=",
  ]
}
"#;

    fn scan_fixture() -> Scan {
        let files = vec![
            ("infra/bootstrap/main.tf".to_owned(), MAIN.to_owned()),
            ("infra/bootstrap/backend.tf".to_owned(), BACKEND.to_owned()),
            ("infra/bootstrap/.terraform.lock.hcl".to_owned(), LOCK.to_owned()),
            ("infra/bootstrap/prod.tfvars".to_owned(), "x = 1".to_owned()),
            ("infra/bootstrap/terraform.tfvars".to_owned(), "x = 1".to_owned()),
            (
                "infra/bootstrap/networking/main.tf".to_owned(),
                "module \"subnets\" {\n  source = \"../../modules/subnets\"\n}\nresource \"aws_vpc\" \"main\" {}\n".to_owned(),
            ),
            (
                "infra/modules/subnets/main.tf".to_owned(),
                "resource \"aws_subnet\" \"a\" {}\n".to_owned(),
            ),
            (
                "infra/shared/db/main.tf".to_owned(),
                "resource \"aws_rds_cluster\" \"db\" {}\n".to_owned(),
            ),
            (
                "modules/unused/main.tf".to_owned(),
                "variable \"x\" {}\n".to_owned(),
            ),
            (
                "envs/prod/main.tf".to_owned(),
                "terraform {\n  cloud {\n    organization = \"acme\"\n  }\n}\nmodule \"net\" {\n  source = \"../../infra/bootstrap/networking\"\n}\n".to_owned(),
            ),
            (
                "infra/bootstrap/.terraform/modules/eks/main.tf".to_owned(),
                "resource \"x\" \"y\" {}".to_owned(),
            ),
        ];
        analyze(files, |p| p == "infra/bootstrap/.terraform")
    }

    #[test]
    fn hcl_blocks_attributes_and_strings() {
        let items = parse_hcl(MAIN);
        let blocks: Vec<(String, Vec<String>)> = items
            .iter()
            .filter_map(|i| match i {
                Item::Block { kind, labels, .. } => Some((kind.clone(), labels.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(blocks.len(), 10, "{blocks:?}");
        assert_eq!(blocks[0], ("module".into(), vec!["networking".into()]));
        assert_eq!(
            blocks[6],
            (
                "resource".into(),
                vec!["aws_s3_bucket".into(), "logs".into()]
            )
        );
        assert_eq!(blocks[9].0, "locals");
        let Item::Block { body, .. } = &items[1] else {
            panic!()
        };
        assert_eq!(attr_str(body, "version"), Some("~> 20.31"));
        assert_eq!(
            attr_str(body, "cluster_name"),
            Some("shop-${var.environment}")
        );
    }

    #[test]
    fn roots_and_modules() {
        let scan = scan_fixture();
        let roots: Vec<&str> = scan.roots.iter().map(|r| r.info.dir.as_str()).collect();
        assert_eq!(roots, ["envs/prod", "infra/bootstrap"]);
        let modules: Vec<&str> = scan.modules.iter().map(|m| m.dir.as_str()).collect();
        assert_eq!(
            modules,
            [
                "infra/bootstrap/networking",
                "infra/modules/subnets",
                "infra/shared/db",
                "modules/unused"
            ]
        );
        let b = &scan.roots[1];
        assert_eq!(
            b.module_dirs,
            [
                "infra/bootstrap/networking",
                "infra/modules/subnets",
                "infra/shared/db"
            ]
        );
        let i = &b.info;
        assert_eq!(i.backend.as_deref(), Some("s3"));
        assert_eq!(i.required_version.as_deref(), Some(">= 1.7.0"));
        assert!(i.mentions_tofu);
        assert!(i.lock_file);
        assert!(i.initialized);
        assert_eq!(
            i.files.len(),
            2,
            ".terraform/ is skipped, lock and tfvars are not config"
        );
        assert_eq!(i.tfvars, ["prod.tfvars", "terraform.tfvars"]);
        assert_eq!(i.auto_tfvars(), ["terraform.tfvars"]);
        let names: Vec<&str> = i.providers.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["aws", "kubernetes", "random"]);
        let aws = &i.providers[0];
        assert_eq!(
            (aws.source.as_str(), aws.version.as_str()),
            ("hashicorp/aws", "~> 5.0")
        );
        assert_eq!(aws.locked.as_deref(), Some("5.31.0"));
        assert_eq!(i.providers[2].version, "~> 3.5", "legacy string form");
        assert_eq!(i.modules.len(), 3);
        assert_eq!(
            i.modules[0].local_dir.as_deref(),
            Some("infra/bootstrap/networking")
        );
        assert_eq!(i.modules[1].local_dir, None);
        assert_eq!(i.modules[2].local_dir.as_deref(), Some("infra/shared/db"));
        assert_eq!(i.resources, ["aws_s3_bucket.logs"]);
        assert_eq!(i.data_sources, 1);
        assert_eq!(i.outputs, 1);
        assert_eq!(i.locals, 2);
        let req: Vec<&str> = i
            .required_variables()
            .iter()
            .map(|v| v.name.as_str())
            .collect();
        assert_eq!(req, ["preview_domain"]);
        let pw = i
            .variables
            .iter()
            .find(|v| v.name == "db_password")
            .unwrap();
        assert!(pw.sensitive && pw.has_default);
        assert!(!i.variables[0].sensitive);
        let prod = &scan.roots[0].info;
        assert_eq!(prod.backend.as_deref(), Some("cloud"));
        assert!(!prod.initialized);
    }

    #[test]
    fn changes_map_to_roots() {
        let scan = scan_fixture();
        let changed = vec![
            "infra/bootstrap/main.tf".to_owned(),
            "infra/modules/subnets/main.tf".to_owned(),
            "README.md".to_owned(),
            "infra/bootstrap/policies/s3.json".to_owned(),
        ];
        let t = touched(&scan.roots, &changed);
        assert_eq!(t.len(), 2);
        let (prod, files) = &t[0];
        assert_eq!(scan.roots[*prod].info.dir, "envs/prod");
        assert_eq!(
            files,
            &["infra/modules/subnets/main.tf"],
            "through networking → subnets"
        );
        let (boot, files) = &t[1];
        assert_eq!(scan.roots[*boot].info.dir, "infra/bootstrap");
        assert_eq!(
            files,
            &[
                "infra/bootstrap/main.tf",
                "infra/modules/subnets/main.tf",
                "infra/bootstrap/policies/s3.json"
            ]
        );
        assert!(touched(&scan.roots, &["docs/x.md".to_owned()]).is_empty());
    }

    #[test]
    fn a_root_at_the_top_only_takes_configuration_files() {
        let scan = analyze(
            vec![
                (
                    "main.tf".into(),
                    "terraform {\n backend \"local\" {}\n}\n".into(),
                ),
                (
                    "nested/main.tf".into(),
                    "terraform {\n backend \"s3\" {}\n}\n".into(),
                ),
            ],
            |_| false,
        );
        assert_eq!(scan.roots.len(), 2);
        let t = touched(
            &scan.roots,
            &[
                "src/app.rs".into(),
                "vars.tfvars".into(),
                "nested/x.tf".into(),
            ],
        );
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].1, ["vars.tfvars"], "nested root files belong to it");
        assert_eq!(t[1].1, ["nested/x.tf"]);
    }

    #[test]
    fn finds_roots_on_disk() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("gitvis-tf-{}-{nanos}", std::process::id()));
        let w = |rel: &str, text: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        w(
            "infra/live/main.tf",
            "module \"vpc\" {\n  source = \"../modules/vpc\"\n}\n",
        );
        w(
            "infra/live/backend.tf",
            "terraform {\n  backend \"gcs\" {}\n}\n",
        );
        w(
            "infra/modules/vpc/main.tf",
            "resource \"google_compute_network\" \"n\" {}\n",
        );
        w(
            "infra/live/.terraform/providers/x.tf",
            "resource \"a\" \"b\" {}\n",
        );
        w("app/readme.md", "nothing");
        std::fs::create_dir_all(root.join("infra/live/.terraform")).unwrap();
        let scan = find_roots(&root).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(scan.roots.len(), 1);
        let r = &scan.roots[0];
        assert_eq!(r.info.dir, "infra/live");
        assert_eq!(r.info.backend.as_deref(), Some("gcs"));
        assert!(r.info.initialized);
        assert_eq!(r.module_dirs, ["infra/modules/vpc"]);
        assert_eq!(scan.modules.len(), 1);
    }

    #[test]
    fn command_lines() {
        assert_eq!(
            Command::Fmt.args(None),
            ["fmt", "-check", "-recursive", "-diff", "-no-color"]
        );
        assert_eq!(
            Command::Init.args(None),
            ["init", "-backend=false", "-input=false", "-no-color"]
        );
        assert_eq!(
            Command::Validate.args(None),
            ["validate", "-json", "-no-color"]
        );
        let plan = Command::Plan.args(Some(Path::new("/tmp/p.tfplan")));
        assert_eq!(
            plan,
            [
                "plan",
                "-input=false",
                "-no-color",
                "-lock=false",
                "-detailed-exitcode",
                "-out=/tmp/p.tfplan"
            ]
        );
        let drift = Command::Drift.args(None);
        assert!(drift.contains(&"-refresh-only".to_owned()));
        assert!(drift.contains(&"-detailed-exitcode".to_owned()));
        assert!(!drift.iter().any(|a| a == "apply"));
        assert!(Command::Init.network() && !Command::Validate.network());
        assert_eq!(
            show_args(Path::new("p.tfplan")),
            ["show", "-json", "-no-color", "p.tfplan"]
        );
        let p = plan_file("infra/bootstrap");
        assert!(
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("gitvis-plan-infra-bootstrap-")
        );
        let s = spec(Binary::Tofu, Path::new("."), Command::Validate, None);
        assert_eq!(s.args, ["validate", "-json", "-no-color"]);
        assert_eq!(Binary::Tofu.word(), "tofu");
    }

    #[test]
    fn validate_json_parses() {
        let out = r#"{
  "format_version": "1.0",
  "valid": false,
  "error_count": 2,
  "warning_count": 1,
  "diagnostics": [
    {
      "severity": "error",
      "summary": "Unsupported argument",
      "detail": "An argument named \"instance_typo\" is not expected here.",
      "range": {
        "filename": "main.tf",
        "start": {"line": 14, "column": 3, "byte": 301},
        "end": {"line": 14, "column": 16, "byte": 314}
      },
      "snippet": {
        "context": "resource \"aws_instance\" \"web\"",
        "code": "  instance_typo = \"t3.micro\"",
        "start_line": 14,
        "highlight_start_offset": 2,
        "highlight_end_offset": 15,
        "values": []
      }
    },
    {
      "severity": "error",
      "summary": "Missing required provider",
      "detail": "This configuration requires provider registry.terraform.io/hashicorp/aws, but that provider isn't available. You may be able to install it automatically by running:\n  terraform init"
    },
    {
      "severity": "warning",
      "summary": "Deprecated attribute",
      "detail": "The attribute \"name\" is deprecated.",
      "range": {"filename": "modules/x/main.tf", "start": {"line": 3, "column": 9}}
    }
  ]
}"#;
        let v = parse_validate(out).unwrap();
        assert!(!v.valid);
        assert_eq!((v.errors, v.warnings), (2, 1));
        let d = &v.diagnostics[0];
        assert!(d.is_error());
        assert_eq!(d.location().as_deref(), Some("main.tf:14:3"));
        assert_eq!(
            d.context.as_deref(),
            Some("resource \"aws_instance\" \"web\"")
        );
        assert_eq!(d.code.as_deref(), Some("  instance_typo = \"t3.micro\""));
        assert_eq!(v.diagnostics[1].location(), None);
        assert_eq!(v.diagnostics[2].severity, "warning");
        let ok = parse_validate(
            "{\"format_version\":\"1.0\",\"valid\":true,\"error_count\":0,\"warning_count\":0,\"diagnostics\":[]}",
        )
        .unwrap();
        assert!(ok.valid && ok.diagnostics.is_empty());
        assert!(parse_validate("Error: something").is_err());
        assert_eq!(
            likely_cause(&v.diagnostics[1].detail, Binary::Terraform).as_deref(),
            Some(
                "Providers are not installed in .terraform/ yet: run Init first (it downloads them)."
            )
        );
    }

    const PLAN: &str = r#"
Note: Objects have changed outside of Terraform

Terraform detected the following changes made outside of Terraform since the
last "terraform apply" which may have affected this plan:

  # aws_security_group.web has changed
  ~ resource "aws_security_group" "web" {
        id                     = "sg-0123"
      ~ tags                   = {
          + "Owner" = "ops"
        }
    }

Unless you have made equivalent changes to your configuration, or ignored the
relevant attributes using ignore_changes, the following plan may include
actions to undo or respond to these changes.

─────────────────────────────────────────────────────────────────────────────

Terraform used the selected providers to generate the following execution
plan. Resource actions are indicated with the following symbols:
  + create
  ~ update in-place
  - destroy
-/+ destroy and then create replacement

Terraform will perform the following actions:

  # aws_instance.web must be replaced
-/+ resource "aws_instance" "web" {
      ~ ami = "ami-1" -> "ami-2" # forces replacement
    }

  # aws_s3_bucket.old will be destroyed
  # (because aws_s3_bucket.old is not in configuration)
  - resource "aws_s3_bucket" "old" {
      - bucket = "old" -> null
    }

  # module.net.aws_subnet.private["a"] will be created
  + resource "aws_subnet" "private" {
      + cidr_block = "10.0.1.0/24"
    }

  # aws_iam_role.ci will be updated in-place
  ~ resource "aws_iam_role" "ci" {
        name = "ci"
    }

  # data.aws_iam_policy_document.ci will be read during apply
  # (config refers to values not yet known)
 <= data "aws_iam_policy_document" "ci" {
    }

Plan: 2 to add, 1 to change, 2 to destroy.

Changes to Outputs:
  + subnet_id = (known after apply)
  ~ web_ip    = "1.2.3.4" -> (known after apply)
  - legacy    = "x" -> null

─────────────────────────────────────────────────────────────────────────────

Saved the plan to: /tmp/p.tfplan
"#;

    #[test]
    fn plan_text_parses() {
        let p = parse_plan_text(PLAN);
        assert_eq!(
            p.counts,
            Some(Counts {
                add: 2,
                change: 1,
                destroy: 2,
                import: 0,
                forget: 0
            })
        );
        assert!(!p.no_changes);
        let a: Vec<(&str, PlanAction)> = p
            .resources
            .iter()
            .map(|r| (r.address.as_str(), r.action))
            .collect();
        assert_eq!(
            a,
            [
                ("aws_instance.web", PlanAction::Replace),
                ("aws_s3_bucket.old", PlanAction::Delete),
                ("module.net.aws_subnet.private[\"a\"]", PlanAction::Create),
                ("aws_iam_role.ci", PlanAction::Update),
                ("data.aws_iam_policy_document.ci", PlanAction::Read),
            ]
        );
        assert_eq!(
            p.resources[1].reason.as_deref(),
            Some("because aws_s3_bucket.old is not in configuration")
        );
        assert_eq!(p.resources[2].module(), Some("module.net"));
        assert_eq!(p.resources[2].resource_type, "aws_subnet");
        assert_eq!(p.resources[4].resource_type, "aws_iam_policy_document");
        assert_eq!(p.drift.len(), 1);
        assert_eq!(p.drift[0].action, PlanAction::Drifted);
        assert_eq!(p.drift[0].address, "aws_security_group.web");
        assert_eq!(
            p.outputs,
            [
                ("subnet_id".to_owned(), "+".to_owned()),
                ("web_ip".to_owned(), "~".to_owned()),
                ("legacy".to_owned(), "-".to_owned())
            ]
        );
        assert!(p.errors.is_empty());
        // Counts without the summary line come from the resource lines.
        let mut no_summary = p.clone();
        no_summary.counts = None;
        assert_eq!(no_summary.counts().add, 2);
        assert_eq!(no_summary.counts().destroy, 2);
    }

    #[test]
    fn plan_text_variants() {
        let p = parse_plan_text(
            "Plan: 1 to import, 0 to add, 0 to change, 0 to destroy, 1 to forget.\n  # aws_x.y will be imported\n  # aws_x.z will no longer be managed by Terraform\n  # aws_x.a has moved to aws_x.b\n",
        );
        let c = p.counts.unwrap();
        assert_eq!((c.import, c.forget, c.total()), (1, 1, 2));
        assert_eq!(p.resources[0].action, PlanAction::Import);
        assert_eq!(p.resources[1].action, PlanAction::Forget);
        assert_eq!(p.resources[2].action, PlanAction::Move);
        assert_eq!(p.resources[2].reason.as_deref(), Some("moved to aws_x.b"));
        let none = parse_plan_text(
            "No changes. Your infrastructure matches the configuration.\n\nTerraform has compared your real infrastructure against your configuration\nand found no differences, so no changes are needed.\n",
        );
        assert!(none.no_changes);
        assert_eq!(none.counts(), Counts::default());
        let drift = parse_plan_text(
            "Note: Objects have changed outside of OpenTofu\n\n  # aws_instance.web has been deleted\n  - resource \"aws_instance\" \"web\" {\n    }\n\nThis is a refresh-only plan, so OpenTofu will not take any actions to undo\nthese.\n",
        );
        assert_eq!(drift.drift.len(), 1);
        assert_eq!(drift.drift[0].action, PlanAction::DriftDeleted);
        assert!(drift.resources.is_empty());
    }

    #[test]
    fn plan_errors_with_box_drawing() {
        let text = "Planning failed. Terraform encountered an error while generating this plan.\n\n╷\n│ Error: No valid credential sources found\n│ \n│   with provider[\"registry.terraform.io/hashicorp/aws\"],\n│   on backend.tf line 25, in provider \"aws\":\n│   25: provider \"aws\" {\n│ \n│ Please see https://registry.terraform.io/providers/hashicorp/aws\n│ for more information about providing credentials.\n╵\n╷\n│ Warning: Argument is deprecated\n╵\n";
        let p = parse_plan_text(text);
        assert_eq!(p.errors.len(), 1);
        let e = &p.errors[0];
        assert_eq!(e.summary, "No valid credential sources found");
        assert_eq!(e.file.as_deref(), Some("backend.tf"));
        assert_eq!(e.line, Some(25));
        assert!(
            e.detail
                .contains("for more information about providing credentials.")
        );
        assert_eq!(p.warnings.len(), 1);
        assert!(
            likely_cause(text, Binary::Terraform)
                .unwrap()
                .contains("credentials")
        );
        let plain = "Error: Backend initialization required, please run \"terraform init\"\n\nReason: Initial configuration of the requested backend \"s3\"\n";
        let (errs, _) = parse_diagnostics(plain);
        assert_eq!(
            errs[0].summary,
            "Backend initialization required, please run \"terraform init\""
        );
        assert!(
            likely_cause(plain, Binary::Terraform)
                .unwrap()
                .contains("-backend=false")
        );
        assert!(likely_cause("Error: No value for required variable", Binary::Tofu).is_some());
        assert_eq!(likely_cause("all good", Binary::Tofu), None);
    }

    #[test]
    fn show_json_parses_without_values() {
        let json = r#"{
  "format_version": "1.2",
  "terraform_version": "1.9.5",
  "planned_values": {"root_module": {}},
  "resource_drift": [
    {"address": "aws_security_group.web", "mode": "managed", "type": "aws_security_group", "name": "web",
     "provider_name": "registry.terraform.io/hashicorp/aws",
     "change": {"actions": ["update"], "before": {"tags": {}}, "after": {"tags": {"Owner": "ops"}}}}
  ],
  "resource_changes": [
    {"address": "aws_instance.web", "mode": "managed", "type": "aws_instance", "name": "web",
     "provider_name": "registry.terraform.io/hashicorp/aws",
     "change": {"actions": ["delete", "create"], "before": {"ami": "ami-1"}, "after": {"ami": "ami-2", "password": "hunter2"},
                "after_sensitive": {"password": true}},
     "action_reason": "replace_because_cannot_update"},
    {"address": "module.net.aws_subnet.private[\"a\"]", "module_address": "module.net", "mode": "managed",
     "type": "aws_subnet", "name": "private", "index": "a",
     "provider_name": "registry.terraform.io/hashicorp/aws",
     "change": {"actions": ["create"], "before": null, "after": {}}},
    {"address": "aws_iam_role.ci", "mode": "managed", "type": "aws_iam_role", "name": "ci",
     "provider_name": "registry.terraform.io/hashicorp/aws",
     "change": {"actions": ["no-op"], "before": {}, "after": {}}},
    {"address": "aws_s3_bucket.imported", "mode": "managed", "type": "aws_s3_bucket", "name": "imported",
     "provider_name": "registry.terraform.io/hashicorp/aws",
     "change": {"actions": ["no-op"], "importing": {"id": "my-bucket"}}},
    {"address": "data.aws_iam_policy_document.ci", "mode": "data", "type": "aws_iam_policy_document", "name": "ci",
     "provider_name": "registry.terraform.io/hashicorp/aws",
     "change": {"actions": ["read"]}, "action_reason": "read_because_config_unknown"}
  ],
  "output_changes": {
    "subnet_id": {"actions": ["create"], "before": null, "after_unknown": true},
    "same": {"actions": ["no-op"]}
  },
  "errored": false,
  "applyable": true,
  "complete": true
}"#;
        let p = parse_show_json(json.as_bytes()).unwrap();
        assert_eq!(p.terraform_version, "1.9.5");
        assert_eq!(p.unchanged, 1);
        let a: Vec<(&str, PlanAction)> = p
            .resources
            .iter()
            .map(|r| (r.address.as_str(), r.action))
            .collect();
        assert_eq!(
            a,
            [
                ("aws_instance.web", PlanAction::Replace),
                ("module.net.aws_subnet.private[\"a\"]", PlanAction::Create),
                ("aws_s3_bucket.imported", PlanAction::Import),
                ("data.aws_iam_policy_document.ci", PlanAction::Read),
            ]
        );
        assert_eq!(
            p.resources[0].reason.as_deref(),
            Some("an argument that forces replacement changed")
        );
        assert_eq!(p.resources[0].provider, "hashicorp/aws");
        assert_eq!(p.drift[0].action, PlanAction::Drifted);
        assert_eq!(p.outputs, [("subnet_id".to_owned(), PlanAction::Create)]);
        let c = p.counts();
        assert_eq!((c.add, c.destroy, c.import), (2, 1, 1));
        // Values never make it into the model.
        assert!(!format!("{p:?}").contains("hunter2"));
        assert!(parse_show_json(b"nope").is_err());
    }

    #[test]
    fn exit_codes() {
        assert_eq!(plan_outcome(Some(0)), PlanOutcome::NoChanges);
        assert_eq!(plan_outcome(Some(2)), PlanOutcome::Changes);
        assert_eq!(plan_outcome(Some(1)), PlanOutcome::Failed);
        assert_eq!(plan_outcome(None), PlanOutcome::Stopped);
        assert_eq!(drift_outcome(Some(2)), DriftOutcome::Drift);
        assert_eq!(drift_outcome(Some(0)), DriftOutcome::InSync);
        assert_eq!(drift_outcome(Some(1)), DriftOutcome::Failed);
        assert_eq!(drift_outcome(None), DriftOutcome::Stopped);
    }

    #[test]
    fn fmt_output_lists_files() {
        let out = "main.tf\n--- old/main.tf\n+++ new/main.tf\n@@ -1,3 +1,3 @@\n resource \"a\" \"b\" {\n-  x=1\n+  x = 1\n }\nmodules/vpc/variables.tf\n--- old/modules/vpc/variables.tf\n+++ new/modules/vpc/variables.tf\n";
        assert_eq!(parse_fmt(out), ["main.tf", "modules/vpc/variables.tf"]);
        assert!(parse_fmt("").is_empty());
    }

    #[test]
    fn paths_and_addresses() {
        assert_eq!(
            normalize("infra/bootstrap/./networking").as_deref(),
            Some("infra/bootstrap/networking")
        );
        assert_eq!(
            normalize("infra/bootstrap/../shared").as_deref(),
            Some("infra/shared")
        );
        assert_eq!(normalize("../outside"), None);
        assert_eq!(resource_type("module.a.module.b.aws_x.y[0]"), "aws_x");
        assert_eq!(
            module_of("module.a.module.b.aws_x.y"),
            Some("module.a.module.b")
        );
        assert_eq!(module_of("aws_x.y"), None);
        assert_eq!(
            parse_lock(LOCK),
            [("hashicorp/aws".to_owned(), "5.31.0".to_owned())]
        );
    }
}
