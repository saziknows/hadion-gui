//! Release helpers: find version manifests and their current version, bump
//! them to the release's version without reformatting anything (only the
//! version text changes), insert a Keep a Changelog section into
//! `CHANGELOG.md` (with its compare links), and make the release commit.
//!
//! Formats: `package.json` (top-level `version`), `Cargo.toml`
//! (`[package]` or `[workspace.package]` `version`), `pyproject.toml`
//! (`[project]` or `[tool.poetry]` `version`), `*.csproj` (`<Version>`),
//! `pom.xml` (the project's own `<version>`, not the parent's), `build.gradle`
//! / `build.gradle.kts` (`version = "…"`), a `VERSION` file, `Chart.yaml`
//! (`version` and `appVersion`).

use std::path::Path;

use regex::Regex;

use crate::{Error, Result, cli};

/// A manifest format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    PackageJson,
    Cargo,
    Pyproject,
    Csproj,
    Pom,
    Gradle,
    VersionFile,
    Chart,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::PackageJson => "npm package",
            Format::Cargo => "Cargo",
            Format::Pyproject => "Python project",
            Format::Csproj => ".NET project",
            Format::Pom => "Maven",
            Format::Gradle => "Gradle",
            Format::VersionFile => "VERSION file",
            Format::Chart => "Helm chart",
        }
    }
}

/// The format of a file by its name (`crates/x/Cargo.toml` → Cargo).
pub fn format_of(path: &str) -> Option<Format> {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    Some(match name {
        "package.json" => Format::PackageJson,
        "Cargo.toml" => Format::Cargo,
        "pyproject.toml" => Format::Pyproject,
        "pom.xml" => Format::Pom,
        "build.gradle" | "build.gradle.kts" => Format::Gradle,
        "VERSION" => Format::VersionFile,
        "Chart.yaml" => Format::Chart,
        n if n.ends_with(".csproj") => Format::Csproj,
        _ => return None,
    })
}

/// One version string in a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    /// `version`, `[workspace.package] version`, `appVersion`, `<Version>`.
    pub label: String,
    pub version: String,
    /// Byte range of the version text (inside quotes).
    pub start: usize,
    pub end: usize,
    /// 1-based line.
    pub line: usize,
}

/// A manifest with its version fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detected {
    /// Relative to the repository, `/`-separated.
    pub path: String,
    pub format: Format,
    pub fields: Vec<Field>,
    /// Why nothing is bumpable (`version.workspace = true`, …).
    pub notes: Vec<String>,
}

fn line_of(content: &str, at: usize) -> usize {
    content[..at.min(content.len())].matches('\n').count() + 1
}

fn field(content: &str, label: &str, start: usize, end: usize) -> Field {
    Field {
        label: label.to_owned(),
        version: content[start..end].to_owned(),
        start,
        end,
        line: line_of(content, start),
    }
}

// --- JSON -------------------------------------------------------------------

fn json_string_end(b: &[u8], mut i: usize) -> Option<usize> {
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return Some(i),
            _ => i += 1,
        }
    }
    None
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Byte range of the string value of a top-level key.
fn json_top_level(content: &str, key: &str) -> Option<(usize, usize)> {
    let b = content.as_bytes();
    let mut depth = 0i32;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                let start = i + 1;
                let end = json_string_end(b, start)?;
                if depth == 1 {
                    let j = skip_ws(b, end + 1);
                    if b.get(j) == Some(&b':') && &content[start..end] == key {
                        let v = skip_ws(b, j + 1);
                        if b.get(v) != Some(&b'"') {
                            return None;
                        }
                        let ve = json_string_end(b, v + 1)?;
                        return Some((v + 1, ve));
                    }
                }
                i = end + 1;
                continue;
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    None
}

// --- TOML -------------------------------------------------------------------

/// Lines with their byte offset.
fn lines_at(content: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut at = 0;
    content.split_inclusive('\n').map(move |l| {
        let start = at;
        at += l.len();
        (start, l.trim_end_matches(['\n', '\r']))
    })
}

fn toml_header(line: &str) -> Option<String> {
    let t = line.trim();
    if t.starts_with("[[") {
        return Some(t.split("]]").next().unwrap_or(t).to_owned() + "]]");
    }
    let inner = t.strip_prefix('[')?;
    let inner = inner.split(']').next()?;
    Some(
        inner
            .split('.')
            .map(|p| p.trim().trim_matches('"'))
            .collect::<Vec<_>>()
            .join("."),
    )
}

fn toml_key_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"^\s*version\s*=\s*(?:"([^"\n]*)"|'([^'\n]*)')"#).unwrap())
}

/// The `version = "…"` of a TOML table; `Err(note)` when the table says it
/// inherits or is dynamic.
fn toml_version(content: &str, table: &str) -> Option<std::result::Result<(usize, usize), String>> {
    let mut current = String::new();
    for (at, line) in lines_at(content) {
        if let Some(h) = toml_header(line) {
            current = h;
            continue;
        }
        if current != table {
            continue;
        }
        let t = line.trim_start();
        if t.starts_with("version.workspace")
            || t.replace(' ', "").starts_with("version={workspace=true")
        {
            return Some(Err(format!("[{table}] inherits the workspace version")));
        }
        if t.starts_with("dynamic") && t.contains("\"version\"") {
            return Some(Err(format!(
                "[{table}] version is dynamic (set by the build backend)"
            )));
        }
        if let Some(c) = toml_key_re().captures(line) {
            let m = c.get(1).or_else(|| c.get(2))?;
            return Some(Ok((at + m.start(), at + m.end())));
        }
    }
    None
}

// --- XML --------------------------------------------------------------------

/// The text of the first element whose path from the root is `path`
/// (`["project", "version"]`), trimmed.
fn xml_text(content: &str, path: &[&str]) -> Option<(usize, usize)> {
    let b = content.as_bytes();
    let mut stack: Vec<String> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        let rest = &content[i..];
        let skip_to = move |pat: &str| rest.find(pat).map(|p| i + p + pat.len());
        if rest.starts_with("<!--") {
            i = skip_to("-->")?;
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            i = skip_to("]]>")?;
            continue;
        }
        if rest.starts_with("<?") || rest.starts_with("<!") {
            i = skip_to(">")?;
            continue;
        }
        let close = i + rest.find('>')?;
        let tag = &content[i + 1..close];
        if let Some(name) = tag.strip_prefix('/') {
            let name = name.trim();
            if let Some(p) = stack.iter().rposition(|s| s == name) {
                stack.truncate(p);
            }
        } else if !tag.ends_with('/') {
            let name = tag
                .split(|c: char| c.is_whitespace())
                .next()
                .unwrap_or("")
                .to_owned();
            let name = name.rsplit(':').next().unwrap_or(&name).to_owned();
            stack.push(name);
            if stack.len() == path.len() && stack.iter().zip(path).all(|(a, b)| a == b) {
                let text_start = close + 1;
                let text_end = text_start + content[text_start..].find('<')?;
                let raw = &content[text_start..text_end];
                let lead = raw.len() - raw.trim_start().len();
                let trimmed = raw.trim();
                return Some((text_start + lead, text_start + lead + trimmed.len()));
            }
        }
        i = close + 1;
    }
    None
}

// --- Detection --------------------------------------------------------------

/// The version fields of one manifest (`None` when the name is not a
/// manifest).
pub fn detect(path: &str, content: &str) -> Option<Detected> {
    let format = format_of(path)?;
    let mut d = Detected {
        path: path.replace('\\', "/"),
        format,
        fields: Vec::new(),
        notes: Vec::new(),
    };
    match format {
        Format::PackageJson => match json_top_level(content, "version") {
            Some((s, e)) => d.fields.push(field(content, "version", s, e)),
            None => d.notes.push("no top-level \"version\"".into()),
        },
        Format::Cargo => {
            for (table, label) in [
                ("package", "[package] version"),
                ("workspace.package", "[workspace.package] version"),
            ] {
                match toml_version(content, table) {
                    Some(Ok((s, e))) => d.fields.push(field(content, label, s, e)),
                    Some(Err(note)) => d.notes.push(note),
                    None => {}
                }
            }
        }
        Format::Pyproject => {
            for (table, label) in [
                ("project", "[project] version"),
                ("tool.poetry", "[tool.poetry] version"),
            ] {
                match toml_version(content, table) {
                    Some(Ok((s, e))) => d.fields.push(field(content, label, s, e)),
                    Some(Err(note)) => d.notes.push(note),
                    None => {}
                }
            }
        }
        Format::Csproj => {
            static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
            let re = RE.get_or_init(|| {
                Regex::new(
                    r"<(Version|VersionPrefix)>\s*([^<\s][^<]*?)\s*</(?:Version|VersionPrefix)>",
                )
                .unwrap()
            });
            if let Some(c) = re.captures(content)
                && let (Some(tag), Some(m)) = (c.get(1), c.get(2))
            {
                d.fields.push(field(
                    content,
                    &format!("<{}>", tag.as_str()),
                    m.start(),
                    m.end(),
                ));
            } else {
                d.notes
                    .push("no <Version> (the version may come from Directory.Build.props)".into());
            }
        }
        Format::Pom => match xml_text(content, &["project", "version"]) {
            Some((s, e)) if s < e => d.fields.push(field(content, "<version>", s, e)),
            _ => d
                .notes
                .push("no project <version> (inherits the parent's)".into()),
        },
        Format::Gradle => {
            static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
            let re = RE.get_or_init(|| {
                Regex::new(r#"^\s*version\s*=?\s*(?:"([^"\n$]+)"|'([^'\n$]+)')\s*(?://.*)?$"#)
                    .unwrap()
            });
            let mut depth = 0i32;
            let mut nested: Option<(usize, usize)> = None;
            for (at, line) in lines_at(content) {
                if let Some(c) = re.captures(line)
                    && let Some(m) = c.get(1).or_else(|| c.get(2))
                {
                    let span = (at + m.start(), at + m.end());
                    if depth == 0 {
                        d.fields.push(field(content, "version", span.0, span.1));
                        break;
                    }
                    nested.get_or_insert(span);
                }
                let code = line.split("//").next().unwrap_or(line);
                depth += code.matches('{').count() as i32 - code.matches('}').count() as i32;
            }
            if d.fields.is_empty() {
                match nested {
                    Some((s, e)) => d.fields.push(field(content, "version (in a block)", s, e)),
                    None => d.notes.push("no version = \"…\" line".into()),
                }
            }
        }
        Format::VersionFile => {
            let trimmed = content.trim();
            let lead = content.len() - content.trim_start().len();
            if !trimmed.is_empty() && !trimmed.contains(char::is_whitespace) {
                d.fields
                    .push(field(content, "version", lead, lead + trimmed.len()));
            } else {
                d.notes.push("not a single version".into());
            }
        }
        Format::Chart => {
            static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
            let re = RE.get_or_init(|| {
                Regex::new(
                    r#"^(version|appVersion):[ \t]*(?:"([^"\n]*)"|'([^'\n]*)'|([^\s#'"][^\s#]*))"#,
                )
                .unwrap()
            });
            for (at, line) in lines_at(content) {
                if let Some(c) = re.captures(line)
                    && let Some(m) = c.get(2).or_else(|| c.get(3)).or_else(|| c.get(4))
                {
                    let key = c.get(1).map(|k| k.as_str()).unwrap_or("version");
                    if d.fields.iter().any(|f| f.label == key) {
                        continue;
                    }
                    d.fields
                        .push(field(content, key, at + m.start(), at + m.end()));
                }
            }
            if d.fields.is_empty() {
                d.notes.push("no top-level version".into());
            }
        }
    }
    Some(d)
}

/// `1.2.0` from a tag (`v1.2.0`, `release-1.2.0` is not accepted); `None`
/// when it does not look like a version.
pub fn version_of_tag(tag: &str) -> Option<String> {
    let t = tag.trim();
    let v = t
        .strip_prefix('v')
        .or_else(|| t.strip_prefix('V'))
        .unwrap_or(t);
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re =
        RE.get_or_init(|| Regex::new(r"^[0-9]+(\.[0-9]+){0,3}([-+][0-9A-Za-z.+-]+)?$").unwrap());
    re.is_match(v).then(|| v.to_owned())
}

/// The new value in the old value's style: a `v` prefix is kept.
pub fn styled(old: &str, new: &str) -> String {
    let new = new.trim_start_matches(['v', 'V']);
    if old.starts_with('v') {
        format!("v{new}")
    } else if old.starts_with('V') {
        format!("V{new}")
    } else {
        new.to_owned()
    }
}

/// Replace the given fields' text with `new` (in each one's style). Only
/// those bytes change.
pub fn bump(content: &str, fields: &[&Field], new: &str) -> String {
    let mut sorted: Vec<&&Field> = fields.iter().collect();
    sorted.sort_by_key(|f| std::cmp::Reverse(f.start));
    let mut out = content.to_owned();
    for f in sorted {
        if f.end <= out.len() && f.start <= f.end {
            out.replace_range(f.start..f.end, &styled(&f.version, new));
        }
    }
    out
}

/// Which fields to bump by default: those at the most common current
/// version (root manifests decide when there are any), unless already at
/// `target`. Returns `(file index, field index)` pairs.
pub fn default_selection(found: &[Detected], target: &str) -> Vec<(usize, usize)> {
    let norm = |v: &str| v.trim_start_matches(['v', 'V']).to_owned();
    let count = |root_only: bool| {
        let mut c: Vec<(String, usize)> = Vec::new();
        for d in found {
            if root_only && d.path.contains('/') {
                continue;
            }
            for f in &d.fields {
                let v = norm(&f.version);
                match c.iter_mut().find(|(x, _)| *x == v) {
                    Some((_, n)) => *n += 1,
                    None => c.push((v, 1)),
                }
            }
        }
        c
    };
    let mut c = count(true);
    if c.is_empty() {
        c = count(false);
    }
    let Some(primary) = c.iter().max_by_key(|(_, n)| *n).map(|(v, _)| v.clone()) else {
        return Vec::new();
    };
    let target = norm(target);
    let mut out = Vec::new();
    for (i, d) in found.iter().enumerate() {
        for (j, f) in d.fields.iter().enumerate() {
            let v = norm(&f.version);
            if v == primary && v != target {
                out.push((i, j));
            }
        }
    }
    out
}

// --- CHANGELOG --------------------------------------------------------------

/// What [`changelog_insert`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangelogEdit {
    pub content: String,
    /// 1-based line of the new heading.
    pub line: usize,
    /// Compare links added or changed.
    pub links: usize,
    pub moved_unreleased: bool,
}

fn is_unreleased(heading: &str) -> bool {
    heading.to_ascii_lowercase().contains("unreleased")
}

/// The version a level-2 heading names (`## [1.1.0] - …`, `## v1.1.0`).
fn heading_version(heading: &str) -> Option<String> {
    let t = heading.trim_start_matches('#').trim();
    let t = t
        .strip_prefix('[')
        .map(|r| r.split(']').next().unwrap_or(r))
        .unwrap_or_else(|| {
            t.split(|c: char| c.is_whitespace() || c == '(')
                .next()
                .unwrap_or(t)
        });
    version_of_tag(t).map(|_| t.to_owned())
}

/// Markdown headings in release notes become level 3 or deeper, so they
/// stay inside the version's section.
pub fn demote_headings(notes: &str) -> String {
    notes
        .lines()
        .map(|l| {
            let hashes = l.chars().take_while(|c| *c == '#').count();
            if (1..3).contains(&hashes) && l[hashes..].starts_with(' ') {
                format!("###{}", &l[hashes..])
            } else {
                l.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The entries under `## [Unreleased]` (trimmed), if there are any.
pub fn unreleased_body(content: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.starts_with("## ") && is_unreleased(l))?;
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.starts_with("## ") || is_link_def(l))
        .map(|p| start + 1 + p)
        .unwrap_or(lines.len());
    let body = lines[start + 1..end].join("\n");
    let body = body.trim();
    (!body.is_empty()).then(|| body.to_owned())
}

fn is_link_def(line: &str) -> bool {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\[[^\]]+\]:\s*\S+").unwrap())
        .is_match(line)
}

fn link_def(line: &str) -> Option<(String, String)> {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let c = RE
        .get_or_init(|| Regex::new(r"^\[([^\]]+)\]:\s*(\S+)").unwrap())
        .captures(line)?;
    Some((c[1].to_owned(), c[2].to_owned()))
}

/// `(prefix, from, to)` of `…/compare/from...to`.
fn compare_parts(url: &str) -> Option<(String, String, String)> {
    let i = url.find("/compare/")? + "/compare/".len();
    let (prefix, range) = url.split_at(i);
    let (from, to) = range.split_once("...").or_else(|| range.split_once(".."))?;
    Some((prefix.to_owned(), from.to_owned(), to.to_owned()))
}

/// Insert `## [version] - date` with `notes` above the previous version
/// (below `## [Unreleased]`), update the compare links when the file has
/// them. With `use_unreleased`, the Unreleased entries become the section's
/// body (the notes are not used) and Unreleased is left empty.
pub fn changelog_insert(
    content: &str,
    version: &str,
    tag: &str,
    date: &str,
    notes: &str,
    use_unreleased: bool,
) -> Result<ChangelogEdit> {
    let eol = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let trailing_newline = content.ends_with('\n');
    let mut lines: Vec<String> = content.lines().map(str::to_owned).collect();
    let version = version.trim_start_matches(['v', 'V']);

    // Existing headings.
    let h2: Vec<usize> = (0..lines.len())
        .filter(|&i| lines[i].starts_with("## "))
        .collect();
    let unreleased = h2.iter().copied().find(|&i| is_unreleased(&lines[i]));
    let previous = h2
        .iter()
        .copied()
        .find(|&i| !is_unreleased(&lines[i]) && heading_version(&lines[i]).is_some());
    for &i in &h2 {
        if let Some(v) = heading_version(&lines[i])
            && v.trim_start_matches(['v', 'V']) == version
        {
            return Err(Error::new(format!(
                "CHANGELOG.md already has a section for {version} (line {})",
                i + 1
            )));
        }
    }
    let prev_version = previous.and_then(|i| heading_version(&lines[i]));
    let v_style = prev_version.as_deref().is_some_and(|v| v.starts_with('v'));
    let heading = format!("## [{}{version}] - {date}", if v_style { "v" } else { "" });

    // The body.
    let mut moved = false;
    let body: String = if use_unreleased {
        let b = unreleased_body(content).unwrap_or_default();
        if let (Some(u), false) = (unreleased, b.is_empty()) {
            let end = lines[u + 1..]
                .iter()
                .position(|l| l.starts_with("## ") || is_link_def(l))
                .map(|p| u + 1 + p)
                .unwrap_or(lines.len());
            lines.drain(u + 1..end);
            lines.insert(u + 1, String::new());
            moved = true;
        }
        b
    } else {
        demote_headings(notes.trim())
    };

    // Where it goes (recomputed: the body move may have shifted lines).
    let at = {
        let h2: Vec<usize> = (0..lines.len())
            .filter(|&i| lines[i].starts_with("## "))
            .collect();
        let prev = h2
            .iter()
            .copied()
            .find(|&i| !is_unreleased(&lines[i]) && heading_version(&lines[i]).is_some());
        let first_link = (0..lines.len()).find(|&i| is_link_def(&lines[i]));
        match prev {
            Some(i) => i,
            None => match h2.iter().copied().find(|&i| is_unreleased(&lines[i])) {
                Some(u) => h2
                    .iter()
                    .copied()
                    .find(|&i| i > u)
                    .or(first_link.filter(|&l| l > u))
                    .unwrap_or(lines.len()),
                None => h2.first().copied().or(first_link).unwrap_or(lines.len()),
            },
        }
    };
    let mut block: Vec<String> = Vec::new();
    if at > 0 && !lines[at - 1].trim().is_empty() {
        block.push(String::new());
    }
    block.push(heading);
    block.push(String::new());
    if !body.is_empty() {
        block.extend(body.lines().map(str::to_owned));
        block.push(String::new());
    }
    if at >= lines.len() {
        // At the end: no blank line after.
        while block.last().is_some_and(|l| l.is_empty()) && block.len() > 1 {
            block.pop();
        }
    }
    let heading_line = at + block.iter().position(|l| l.starts_with("## ")).unwrap_or(0) + 1;
    for (k, l) in block.into_iter().enumerate() {
        lines.insert(at + k, l);
    }

    // Compare links.
    let mut links = 0;
    let unreleased_link = (0..lines.len()).find(|&i| {
        link_def(&lines[i])
            .is_some_and(|(name, url)| is_unreleased(&name) && compare_parts(&url).is_some())
    });
    let label = if v_style {
        format!("v{version}")
    } else {
        version.to_owned()
    };
    if let Some(i) = unreleased_link {
        let (name, url) = link_def(&lines[i]).unwrap_or_default();
        if let Some((prefix, from, to)) = compare_parts(&url) {
            lines[i] = format!("[{name}]: {prefix}{tag}...{to}");
            lines.insert(i + 1, format!("[{label}]: {prefix}{from}...{tag}"));
            links = 2;
        }
    } else if let Some(prev) = &prev_version {
        let found = (0..lines.len()).find(|&i| {
            link_def(&lines[i])
                .is_some_and(|(name, url)| name == *prev && compare_parts(&url).is_some())
        });
        if let Some(i) = found
            && let Some((_, url)) = link_def(&lines[i])
            && let Some((prefix, _, to)) = compare_parts(&url)
        {
            lines.insert(i, format!("[{label}]: {prefix}{to}...{tag}"));
            links = 1;
        }
    }

    let mut out = lines.join(eol);
    if trailing_newline || content.is_empty() {
        out.push_str(eol);
    }
    Ok(ChangelogEdit {
        content: out,
        line: heading_line,
        links,
        moved_unreleased: moved,
    })
}

/// A new `CHANGELOG.md` in Keep a Changelog style with one section.
pub fn new_changelog(version: &str, date: &str, notes: &str) -> String {
    let version = version.trim_start_matches(['v', 'V']);
    let mut s = String::from(
        "# Changelog\n\nAll notable changes to this project will be documented in this file.\n\n\
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),\n\
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).\n\n",
    );
    s.push_str(&format!("## [{version}] - {date}\n"));
    let body = demote_headings(notes.trim());
    if !body.is_empty() {
        s.push('\n');
        s.push_str(&body);
        s.push('\n');
    }
    s
}

// --- The plan ---------------------------------------------------------------

/// One file the release commit changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEdit {
    /// Relative, `/`-separated.
    pub path: String,
    /// The content the preview was made from (`None`: a new file).
    pub old: Option<String>,
    pub new: String,
    /// `version 1.1.0 → 1.2.0`, `new section at line 8, 2 links`.
    pub what: String,
}

/// Unified diff of one edit, for the confirmation.
pub fn unified(edit: &FileEdit) -> String {
    let old = edit.old.as_deref().unwrap_or("");
    let a = if edit.old.is_some() {
        format!("a/{}", edit.path)
    } else {
        "/dev/null".to_owned()
    };
    similar::TextDiff::from_lines(old, &edit.new)
        .unified_diff()
        .context_radius(2)
        .header(&a, &format!("b/{}", edit.path))
        .to_string()
}

/// The commit message from a template (`{version}`, `{tag}`).
pub fn commit_message(template: &str, version: &str, tag: &str) -> String {
    let t = if template.trim().is_empty() {
        "chore(release): v{version}"
    } else {
        template.trim()
    };
    t.replace("{version}", version.trim_start_matches(['v', 'V']))
        .replace("{tag}", tag)
}

/// The git commands [`apply`] runs, for the confirmation.
pub fn commands(edits: &[FileEdit], message: &str) -> Vec<String> {
    let paths: Vec<&str> = edits.iter().map(|e| e.path.as_str()).collect();
    let mut add = vec!["add", "--"];
    add.extend(paths.iter().copied());
    let mut commit = vec!["commit", "-m", message, "--"];
    commit.extend(paths.iter().copied());
    vec![
        format!("git {}", crate::devops::proc::render_args(&add)),
        format!("git {}", crate::devops::proc::render_args(&commit)),
    ]
}

/// Write the files (each must still be what the preview showed), stage
/// them and commit only them. Returns `abc1234 chore(release): v1.2.0`.
pub fn apply(
    dir: &Path,
    edits: &[FileEdit],
    message: &str,
    log: &mut Vec<String>,
) -> Result<String> {
    if edits.is_empty() {
        return Err(Error::new("Nothing to change"));
    }
    for e in edits {
        if e.path.contains("..") || Path::new(&e.path).is_absolute() {
            return Err(Error::new(format!("{}: not inside the repository", e.path)));
        }
        let path = dir.join(&e.path);
        let now = std::fs::read_to_string(&path).ok();
        if now != e.old {
            return Err(Error::new(format!(
                "{} changed since the preview: review the release commit again",
                e.path
            )));
        }
    }
    for e in edits {
        let path = dir.join(&e.path);
        std::fs::write(&path, &e.new).map_err(|err| Error::new(format!("{}: {err}", e.path)))?;
        log.push(format!("write {} ({} bytes)", e.path, e.new.len()));
    }
    let paths: Vec<&str> = edits.iter().map(|e| e.path.as_str()).collect();
    let mut add = vec!["add", "--"];
    add.extend(paths.iter().copied());
    log.push(format!("git {}", crate::devops::proc::render_args(&add)));
    cli::run(dir, &add, None)?;
    let mut commit = vec!["commit", "-m", message, "--"];
    commit.extend(paths.iter().copied());
    log.push(format!("git {}", crate::devops::proc::render_args(&commit)));
    cli::run(dir, &commit, None).map_err(|e| {
        Error::new(format!(
            "The files were written and staged, but the commit failed: {e}"
        ))
    })?;
    log.push("git log -1 --format=%h %s".into());
    let out = cli::run(dir, &["log", "-1", "--format=%h %s"], None)?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Folders never searched for manifests.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "vendor",
    "third_party",
    "third-party",
    "target",
    "dist",
    "build",
    "out",
    ".git",
    "testdata",
    "fixtures",
    "__fixtures__",
    "examples",
];

/// At most this many manifests are listed.
pub const MAX_MANIFESTS: usize = 40;

/// Tracked manifests (up to four folders deep) with their versions, and the
/// root `CHANGELOG.md` if there is one (`(path, content)`).
pub fn discover(
    dir: &Path,
    log: &mut Vec<String>,
) -> Result<(Vec<Detected>, Option<(String, String)>)> {
    log.push("git ls-files -z".into());
    let out = cli::run(dir, &["ls-files", "-z"], None)?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut paths: Vec<&str> = text
        .split('\0')
        .filter(|p| !p.is_empty())
        .filter(|p| format_of(p).is_some())
        .filter(|p| p.matches('/').count() <= 4)
        .filter(|p| {
            let parts: Vec<&str> = p.split('/').collect();
            !parts[..parts.len() - 1]
                .iter()
                .any(|d| SKIP_DIRS.contains(&d.to_ascii_lowercase().as_str()))
        })
        .collect();
    paths.sort_by_key(|p| (p.matches('/').count(), p.to_ascii_lowercase()));
    paths.truncate(MAX_MANIFESTS);
    let mut found = Vec::new();
    for p in paths {
        let Ok(content) = std::fs::read_to_string(dir.join(p)) else {
            continue;
        };
        if content.len() > 1 << 20 {
            continue;
        }
        if let Some(d) = detect(p, &content) {
            found.push(d);
        }
    }
    let changelog = text
        .split('\0')
        .find(|p| p.eq_ignore_ascii_case("CHANGELOG.md"))
        .and_then(|p| {
            std::fs::read_to_string(dir.join(p))
                .ok()
                .map(|c| (p.to_owned(), c))
        });
    Ok((found, changelog))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(path: &str, content: &str) -> Detected {
        detect(path, content).unwrap()
    }

    /// Bump every field and check that only the version text changed.
    fn bumped(path: &str, content: &str, new: &str) -> String {
        let d = one(path, content);
        let fields: Vec<&Field> = d.fields.iter().collect();
        let out = bump(content, &fields, new);
        // Same number of lines, same text outside the fields.
        assert_eq!(out.lines().count(), content.lines().count());
        out
    }

    #[test]
    fn package_json() {
        let c = "{\n  \"name\": \"web\",\n  \"version\": \"1.1.0\",\n  \"dependencies\": {\n    \"version\": \"9.9.9\",\n    \"x\": \"^1.0.0\"\n  },\n  \"files\": [\"version\"]\n}\n";
        let d = one("package.json", c);
        assert_eq!(d.fields.len(), 1);
        assert_eq!(d.fields[0].version, "1.1.0");
        assert_eq!(d.fields[0].line, 3);
        let out = bumped("package.json", c, "1.2.0");
        assert_eq!(
            out,
            c.replace("\"version\": \"1.1.0\"", "\"version\": \"1.2.0\"")
        );
        // Version after a nested one, odd spacing, escaped quotes before it.
        let c2 = "{\"scripts\":{\"version\":\"x\"},\"desc\":\"a \\\"q\\\" b\",\"version\" :   \"0.1.0-rc.1\"}";
        let d2 = one("web/package.json", c2);
        assert_eq!(d2.fields[0].version, "0.1.0-rc.1");
        assert_eq!(
            bumped("web/package.json", c2, "0.2.0"),
            c2.replace("0.1.0-rc.1", "0.2.0")
        );
        let none = one("package.json", "{\"private\": true}");
        assert!(none.fields.is_empty() && !none.notes.is_empty());
    }

    #[test]
    fn cargo_toml() {
        let c = "[package]\nname = \"x\" # the name\nversion = \"1.1.0\"   # keep me\nedition = \"2024\"\n\n[dependencies]\nserde = { version = \"1\" }\nversion = \"9\"\n";
        let d = one("Cargo.toml", c);
        assert_eq!(d.fields.len(), 1);
        assert_eq!(d.fields[0].label, "[package] version");
        assert_eq!(
            bumped("Cargo.toml", c, "1.2.0"),
            c.replace("version = \"1.1.0\"", "version = \"1.2.0\"")
        );
        let ws = "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.package]\nversion = '0.4.2'\nedition = \"2024\"\n";
        let d = one("Cargo.toml", ws);
        assert_eq!(d.fields[0].label, "[workspace.package] version");
        assert_eq!(
            bumped("Cargo.toml", ws, "0.5.0"),
            ws.replace("0.4.2", "0.5.0")
        );
        let member = "[package]\nname = \"m\"\nversion.workspace = true\n";
        let d = one("crates/m/Cargo.toml", member);
        assert!(d.fields.is_empty());
        assert_eq!(d.notes, vec!["[package] inherits the workspace version"]);
        let crlf = "[package]\r\nname = \"x\"\r\nversion = \"1.0.0\"\r\n";
        assert_eq!(
            bumped("Cargo.toml", crlf, "2.0.0"),
            crlf.replace("1.0.0", "2.0.0")
        );
    }

    #[test]
    fn pyproject() {
        let pep = "[build-system]\nrequires = [\"hatchling\"]\n\n[project]\nname = \"pkg\"\nversion = \"0.3.1\"\n";
        assert_eq!(
            one("pyproject.toml", pep).fields[0].label,
            "[project] version"
        );
        assert_eq!(
            bumped("pyproject.toml", pep, "0.4.0"),
            pep.replace("0.3.1", "0.4.0")
        );
        let poetry = "[tool.poetry]\nname = \"pkg\"\nversion = \"2.0.0\"\n\n[tool.poetry.dependencies]\npython = \"^3.11\"\n";
        assert_eq!(
            one("pyproject.toml", poetry).fields[0].label,
            "[tool.poetry] version"
        );
        let dynamic = "[project]\nname = \"pkg\"\ndynamic = [\"version\"]\n";
        let d = one("pyproject.toml", dynamic);
        assert!(d.fields.is_empty());
        assert!(d.notes[0].contains("dynamic"));
    }

    #[test]
    fn csproj() {
        let c = "<Project Sdk=\"Microsoft.NET.Sdk\">\r\n  <PropertyGroup>\r\n    <TargetFramework>net8.0</TargetFramework>\r\n    <Version>1.4.0</Version>\r\n  </PropertyGroup>\r\n</Project>\r\n";
        let d = one("src/App/App.csproj", c);
        assert_eq!(d.fields[0].version, "1.4.0");
        assert_eq!(d.fields[0].label, "<Version>");
        assert_eq!(
            bumped("src/App/App.csproj", c, "1.5.0"),
            c.replace("1.4.0", "1.5.0")
        );
        let prefix = "<Project><PropertyGroup><VersionPrefix> 2.0.0 </VersionPrefix></PropertyGroup></Project>";
        let d = one("a.csproj", prefix);
        assert_eq!(d.fields[0].version, "2.0.0");
        assert_eq!(
            bumped("a.csproj", prefix, "2.1.0"),
            prefix.replace("2.0.0", "2.1.0")
        );
        assert!(one("b.csproj", "<Project/>").fields.is_empty());
    }

    #[test]
    fn pom() {
        let c = r#"<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <!-- <version>0.0.0</version> -->
  <modelVersion>4.0.0</modelVersion>
  <parent>
    <groupId>org.example</groupId>
    <version>9.9.9</version>
  </parent>
  <artifactId>app</artifactId>
  <version>1.0.0-SNAPSHOT</version>
  <dependencies>
    <dependency><artifactId>x</artifactId><version>3.2.1</version></dependency>
  </dependencies>
</project>
"#;
        let d = one("pom.xml", c);
        assert_eq!(d.fields.len(), 1);
        assert_eq!(d.fields[0].version, "1.0.0-SNAPSHOT");
        assert_eq!(d.fields[0].line, 10);
        assert_eq!(
            bumped("pom.xml", c, "1.0.0"),
            c.replace(
                "<version>1.0.0-SNAPSHOT</version>",
                "<version>1.0.0</version>"
            )
        );
        let inherits =
            "<project><parent><version>2.0</version></parent><artifactId>a</artifactId></project>";
        let d = one("m/pom.xml", inherits);
        assert!(d.fields.is_empty());
        assert!(d.notes[0].contains("parent"));
    }

    #[test]
    fn gradle() {
        let groovy = "plugins {\n    id 'java'\n}\n\ngroup = 'org.example'\nversion = '1.0.2'\n\ndependencies {\n    version = '9'\n}\n";
        let d = one("build.gradle", groovy);
        assert_eq!(d.fields[0].version, "1.0.2");
        assert_eq!(
            bumped("build.gradle", groovy, "1.1.0"),
            groovy.replace("'1.0.2'", "'1.1.0'")
        );
        let kts = "plugins { kotlin(\"jvm\") version \"2.0.0\" }\nversion = \"0.9.0\" // app\n";
        let d = one("app/build.gradle.kts", kts);
        assert_eq!(d.fields[0].version, "0.9.0");
        assert_eq!(
            bumped("app/build.gradle.kts", kts, "1.0.0"),
            kts.replace("0.9.0", "1.0.0")
        );
        let nested = "allprojects {\n    version = \"3.1.0\"\n}\n";
        let d = one("build.gradle.kts", nested);
        assert_eq!(d.fields[0].label, "version (in a block)");
        let computed = "version = findProperty(\"v\")\n";
        assert!(one("build.gradle", computed).fields.is_empty());
        let templated = "version = \"${rootProject.version}\"\n";
        assert!(one("build.gradle", templated).fields.is_empty());
    }

    #[test]
    fn version_file() {
        assert_eq!(bumped("VERSION", "1.2.3\n", "1.3.0"), "1.3.0\n");
        assert_eq!(bumped("VERSION", "v1.2.3", "1.3.0"), "v1.3.0");
        assert_eq!(bumped("tools/VERSION", "  0.1\r\n", "0.2"), "  0.2\r\n");
        assert!(one("VERSION", "two words\n").fields.is_empty());
    }

    #[test]
    fn chart_yaml() {
        let c = "apiVersion: v2\nname: web\nversion: 0.3.0\nappVersion: \"v1.1.0\" # the image tag\ndependencies:\n  - name: redis\n    version: 17.0.0\n";
        let d = one("charts/web/Chart.yaml", c);
        let labels: Vec<&str> = d.fields.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(labels, vec!["version", "appVersion"]);
        assert_eq!(d.fields[1].version, "v1.1.0");
        let out = bumped("charts/web/Chart.yaml", c, "1.2.0");
        assert_eq!(
            out,
            c.replace("version: 0.3.0", "version: 1.2.0")
                .replace("\"v1.1.0\"", "\"v1.2.0\"")
        );
        // Only appVersion.
        let f: Vec<&Field> = d
            .fields
            .iter()
            .filter(|f| f.label == "appVersion")
            .collect();
        assert_eq!(bump(c, &f, "2.0.0"), c.replace("\"v1.1.0\"", "\"v2.0.0\""));
    }

    #[test]
    fn names_and_versions() {
        assert_eq!(format_of("a/b/Cargo.toml"), Some(Format::Cargo));
        assert_eq!(format_of("x\\App.csproj"), Some(Format::Csproj));
        assert_eq!(format_of("Cargo.lock"), None);
        assert_eq!(format_of("version.txt"), None);
        assert_eq!(version_of_tag("v1.2.0").as_deref(), Some("1.2.0"));
        assert_eq!(version_of_tag("1.2.0-rc.1").as_deref(), Some("1.2.0-rc.1"));
        assert_eq!(version_of_tag("2024.10").as_deref(), Some("2024.10"));
        assert_eq!(version_of_tag("release-1"), None);
        assert_eq!(version_of_tag(""), None);
        assert_eq!(styled("v1.0", "1.1"), "v1.1");
        assert_eq!(styled("1.0", "v1.1"), "1.1");
    }

    #[test]
    fn default_selection_follows_the_root() {
        let found = vec![
            one("package.json", "{\"version\": \"1.1.0\"}"),
            one("Cargo.toml", "[package]\nversion = \"1.1.0\"\n"),
            one("docs/package.json", "{\"version\": \"0.0.1\"}"),
            one("crates/a/Cargo.toml", "[package]\nversion = \"1.1.0\"\n"),
        ];
        assert_eq!(
            default_selection(&found, "v1.2.0"),
            vec![(0, 0), (1, 0), (3, 0)]
        );
        assert!(
            default_selection(&found, "1.1.0").is_empty(),
            "already there"
        );
    }

    const KAC: &str = "# Changelog\n\nAll notable changes.\n\n## [Unreleased]\n\n### Added\n- Groups\n\n## [1.1.0] - 2026-09-01\n\n### Fixed\n- A crash\n\n## [1.0.0] - 2026-08-01\n\n- First\n\n[Unreleased]: https://github.com/o/r/compare/v1.1.0...HEAD\n[1.1.0]: https://github.com/o/r/compare/v1.0.0...v1.1.0\n[1.0.0]: https://github.com/o/r/releases/tag/v1.0.0\n";

    #[test]
    fn changelog_above_the_previous_version() {
        let e = changelog_insert(
            KAC,
            "1.2.0",
            "v1.2.0",
            "2026-10-27",
            "## What's Changed\n- Notifications",
            false,
        )
        .unwrap();
        let expected = "# Changelog\n\nAll notable changes.\n\n## [Unreleased]\n\n### Added\n- Groups\n\n## [1.2.0] - 2026-10-27\n\n### What's Changed\n- Notifications\n\n## [1.1.0] - 2026-09-01\n\n### Fixed\n- A crash\n\n## [1.0.0] - 2026-08-01\n\n- First\n\n[Unreleased]: https://github.com/o/r/compare/v1.2.0...HEAD\n[1.2.0]: https://github.com/o/r/compare/v1.1.0...v1.2.0\n[1.1.0]: https://github.com/o/r/compare/v1.0.0...v1.1.0\n[1.0.0]: https://github.com/o/r/releases/tag/v1.0.0\n";
        assert_eq!(e.content, expected);
        assert_eq!(e.line, 10);
        assert_eq!(e.links, 2);
        assert!(!e.moved_unreleased);
        // Same version twice is refused.
        let again = changelog_insert(&e.content, "1.2.0", "v1.2.0", "2026-10-28", "x", false);
        assert!(
            again
                .unwrap_err()
                .to_string()
                .contains("already has a section")
        );
    }

    #[test]
    fn changelog_moves_unreleased_entries() {
        assert_eq!(unreleased_body(KAC).as_deref(), Some("### Added\n- Groups"));
        let e = changelog_insert(KAC, "1.2.0", "v1.2.0", "2026-10-27", "ignored", true).unwrap();
        assert!(e.moved_unreleased);
        assert!(
            e.content.contains(
                "## [Unreleased]\n\n## [1.2.0] - 2026-10-27\n\n### Added\n- Groups\n\n## [1.1.0]"
            ),
            "{}",
            e.content
        );
        assert!(!e.content.contains("ignored"));
        assert_eq!(unreleased_body(&e.content), None);
    }

    #[test]
    fn changelog_without_unreleased_or_links() {
        let c = "# Changes\r\n\r\n## [v0.9.0] - 2026-01-01\r\n- Old\r\n";
        let e = changelog_insert(c, "1.0.0", "v1.0.0", "2026-10-27", "- New", false).unwrap();
        assert_eq!(
            e.content,
            "# Changes\r\n\r\n## [v1.0.0] - 2026-10-27\r\n\r\n- New\r\n\r\n## [v0.9.0] - 2026-01-01\r\n- Old\r\n"
        );
        assert_eq!(e.links, 0);
        // Only a previous version link: the new one goes above it.
        let c = "# Changelog\n\n## [1.1.0] - 2026-09-01\n- x\n\n[1.1.0]: https://gitlab.com/o/r/-/compare/v1.0.0...v1.1.0\n";
        let e = changelog_insert(c, "1.2.0", "v1.2.0", "2026-10-27", "", false).unwrap();
        assert!(
            e.content.contains("## [1.2.0] - 2026-10-27\n\n## [1.1.0]"),
            "{}",
            e.content
        );
        assert!(
            e.content
                .contains("[1.2.0]: https://gitlab.com/o/r/-/compare/v1.1.0...v1.2.0\n[1.1.0]:")
        );
        assert_eq!(e.links, 1);
        // No version yet: after Unreleased.
        let c = "# Changelog\n\n## [Unreleased]\n\n- wip\n";
        let e = changelog_insert(c, "0.1.0", "v0.1.0", "2026-10-27", "- First", false).unwrap();
        assert_eq!(
            e.content,
            "# Changelog\n\n## [Unreleased]\n\n- wip\n\n## [0.1.0] - 2026-10-27\n\n- First\n"
        );
        // Only a title.
        let e = changelog_insert(
            "# Changelog\n",
            "0.1.0",
            "v0.1.0",
            "2026-10-27",
            "- First",
            false,
        )
        .unwrap();
        assert_eq!(
            e.content,
            "# Changelog\n\n## [0.1.0] - 2026-10-27\n\n- First\n"
        );
    }

    #[test]
    fn new_changelog_and_messages() {
        let c = new_changelog("v1.0.0", "2026-10-27", "# Big\n- a");
        assert!(c.starts_with("# Changelog\n"));
        assert!(c.ends_with("## [1.0.0] - 2026-10-27\n\n### Big\n- a\n"));
        assert_eq!(demote_headings("## A\n#### B\n#tag"), "### A\n#### B\n#tag");
        assert_eq!(
            commit_message("chore(release): v{version}", "v1.2.0", "v1.2.0"),
            "chore(release): v1.2.0"
        );
        assert_eq!(
            commit_message("", "1.2.0", "v1.2.0"),
            "chore(release): v1.2.0"
        );
        assert_eq!(
            commit_message("Release {tag}", "1.2.0", "v1.2.0"),
            "Release v1.2.0"
        );
    }

    #[test]
    fn diffs_and_commands() {
        let e = FileEdit {
            path: "package.json".into(),
            old: Some("{\n  \"version\": \"1.1.0\"\n}\n".into()),
            new: "{\n  \"version\": \"1.2.0\"\n}\n".into(),
            what: String::new(),
        };
        let d = unified(&e);
        assert!(d.contains("--- a/package.json"));
        assert!(d.contains("-  \"version\": \"1.1.0\""));
        assert!(d.contains("+  \"version\": \"1.2.0\""));
        let n = FileEdit {
            path: "CHANGELOG.md".into(),
            old: None,
            new: "# Changelog\n".into(),
            what: String::new(),
        };
        assert!(unified(&n).contains("--- /dev/null"));
        let c = commands(&[e, n], "chore(release): v1.2.0");
        assert_eq!(c[0], "git add -- package.json CHANGELOG.md");
        assert_eq!(
            c[1],
            "git commit -m \"chore(release): v1.2.0\" -- package.json CHANGELOG.md"
        );
    }
}
