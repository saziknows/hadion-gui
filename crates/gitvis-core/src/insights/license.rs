//! Licensing: what the project is licensed under, what its dependencies are
//! licensed under (found offline: lockfiles with licence fields,
//! `node_modules`, the local cargo registry, vendored trees), and copyright
//! headers that name other projects or holders.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;

use regex::Regex;

use super::debt::{MAX_TEXT_BYTES, scan_parallel, text_candidates};
use super::{Progress, Severity, TrackedFile};
use crate::secrets::basename;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectLicense {
    pub path: String,
    /// SPDX id when recognised (`MIT`, `Apache-2.0`, `GPL-3.0-only`,
    /// `BSD-3-Clause`, `MPL-2.0`, `ISC`, `Unlicense`, `AGPL-3.0-only`,
    /// `LGPL-2.1-only`, `CC0-1.0`, `0BSD`, `BSL-1.0`…), else `Unknown`.
    pub id: String,
    /// `exact`, `header`, `keywords`.
    pub confidence: &'static str,
    /// Copyright holders named in the file.
    pub holders: Vec<String>,
    /// Earliest and latest copyright years in the file.
    pub years: Option<(u32, u32)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepLicense {
    pub name: String,
    pub version: String,
    /// `npm`, `cargo`, `pypi`, `go`, `composer`, `rubygems`, `vendor`.
    pub ecosystem: &'static str,
    pub license: Option<String>,
    /// Where it was read: `package-lock.json`, `node_modules`,
    /// `cargo registry`, `vendor LICENSE`, `go.sum (unknown)`.
    pub source: String,
}

/// Files whose header names a copyright holder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HolderGroup {
    pub holder: String,
    pub files: usize,
    pub examples: Vec<String>,
    /// SPDX ids or licence names seen in the same headers.
    pub licenses: Vec<String>,
    pub years: Option<(u32, u32)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LicenseFinding {
    pub severity: Severity,
    /// `no-license`, `copyleft-dependency`, `agpl-dependency`,
    /// `incompatible-dependency`, `unknown-dependency-licenses`,
    /// `foreign-copyright`, `multiple-licenses`, `license-mismatch`
    /// (manifest says X, LICENSE file is Y), `stale-copyright-year`.
    pub id: &'static str,
    pub title: String,
    pub detail: String,
    pub why: &'static str,
    pub paths: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Licensing {
    pub project: Vec<ProjectLicense>,
    /// Licence declared by manifests: `(file, value)` (`package.json`
    /// `license`, `Cargo.toml` `license`, `pyproject.toml`, `composer.json`,
    /// `*.gemspec`, `setup.py`).
    pub declared: Vec<(String, String)>,
    pub dependencies: Vec<DepLicense>,
    /// `(license, count)` desc; unknown counted under `Unknown`.
    pub dep_summary: Vec<(String, u32)>,
    pub copyleft: Vec<DepLicense>,
    /// Holders seen in file headers, desc by files.
    pub holders: Vec<HolderGroup>,
    /// Holders other than the project's own (the top holder and anything
    /// matching the LICENSE file's holders), desc by files.
    pub foreign: Vec<HolderGroup>,
    /// `(SPDX id, files)` from `SPDX-License-Identifier:` headers.
    pub spdx_headers: Vec<(String, usize)>,
    pub files_with_headers: usize,
    pub files_scanned: usize,
    pub findings: Vec<LicenseFinding>,
}

impl Licensing {
    /// The project's main licence id, if one was recognised.
    pub fn main(&self) -> Option<&str> {
        self.project
            .iter()
            .find(|p| p.id != "Unknown")
            .map(|p| p.id.as_str())
    }
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

/// Lower-case, single-spaced.
fn normalise(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// `(id, title phrase)` of the licence whose distinctive phrases `n`
/// (normalised) contains. Order matters: AGPL and LGPL texts quote the GPL,
/// BSL quotes the MIT grant.
fn identify(n: &str) -> Option<(&'static str, &'static str)> {
    let has = |p: &str| n.contains(p);
    if has("server side public license") {
        return Some(("SSPL-1.0", "server side public license"));
    }
    if has("business source license") {
        return Some(("BUSL-1.1", "business source license"));
    }
    if has("elastic license") && has("2.0") {
        return Some(("Elastic-2.0", "elastic license"));
    }
    // The EUPL appendix lists the GPL, LGPL, AGPL and MPL as compatible
    // licences, and the MPL names them as "Secondary Licenses": decide
    // those first.
    if let Some(pos) = n
        .find("european union public licence")
        .or_else(|| n.find("european union public license"))
    {
        let id = match version_after(n, pos).as_deref() {
            Some("1.1") => "EUPL-1.1",
            _ => "EUPL-1.2",
        };
        return Some((id, "european union public licence"));
    }
    if let Some(pos) = n.find("mozilla public license") {
        let id = match version_after(n, pos).as_deref() {
            Some("1.1") => "MPL-1.1",
            Some("1.0") | Some("1") => "MPL-1.0",
            _ => "MPL-2.0",
        };
        return Some((id, "mozilla public license"));
    }
    // The GNU family quotes each other (GPL-3 §13 names the AGPL, the LGPL
    // incorporates the GPL): the title that comes first is the licence.
    let gnu = [
        (
            "agpl",
            n.find("gnu affero general public license")
                .or_else(|| n.find("affero general public license")),
        ),
        (
            "lgpl",
            n.find("gnu lesser general public license")
                .or_else(|| n.find("gnu library general public license"))
                .or_else(|| n.find("lesser general public license")),
        ),
        (
            "gpl",
            n.find("gnu general public license")
                .or_else(|| n.find("gnu gpl")),
        ),
    ];
    if let Some((family, Some(pos))) = gnu
        .iter()
        .filter(|(_, p)| p.is_some())
        .min_by_key(|(_, p)| p.unwrap_or(usize::MAX))
    {
        let v = version_after(n, *pos);
        let id = match (*family, v.as_deref()) {
            ("agpl", Some("1" | "1.0")) => "AGPL-1.0-only",
            ("agpl", _) => "AGPL-3.0-only",
            ("lgpl", Some("3" | "3.0")) => "LGPL-3.0-only",
            ("lgpl", Some("2" | "2.0")) => "LGPL-2.0-only",
            ("lgpl", _) => "LGPL-2.1-only",
            (_, Some("2" | "2.0")) => "GPL-2.0-only",
            (_, Some("1" | "1.0")) => "GPL-1.0-only",
            _ => "GPL-3.0-only",
        };
        let title = match *family {
            "agpl" => "gnu affero general public license",
            "lgpl" => "lesser general public license",
            _ => "gnu general public license",
        };
        return Some((id, title));
    }
    if has("apache license") && (has("version 2.0") || has("version 2,") || has("2.0")) {
        return Some(("Apache-2.0", "apache license"));
    }
    if has("boost software license") {
        return Some(("BSL-1.0", "boost software license"));
    }
    if has("this is free and unencumbered software") {
        return Some(("Unlicense", "this is free and unencumbered software"));
    }
    if has("creative commons zero") || has("cc0 1.0") || has("cc0") {
        return Some(("CC0-1.0", "cc0"));
    }
    if has("do what the fuck you want") || has("wtfpl") {
        return Some(("WTFPL", "do what the fuck you want"));
    }
    if has("altered source versions must be plainly marked") {
        return Some(("Zlib", "zlib"));
    }
    if has("redistribution and use in source and binary forms") {
        let id = if has("all advertising materials") {
            "BSD-4-Clause"
        } else if has("neither the name") {
            "BSD-3-Clause"
        } else {
            "BSD-2-Clause"
        };
        return Some((id, "redistribution and use in source and binary forms"));
    }
    if has("permission is hereby granted, free of charge") {
        return Some(("MIT", "mit license"));
    }
    if has(
        "permission to use, copy, modify, and/or distribute this software for any purpose with or without fee",
    ) || has(
        "permission to use, copy, modify, and distribute this software for any purpose with or without fee",
    ) {
        let id = if has("provided that the above copyright notice") {
            "ISC"
        } else {
            "0BSD"
        };
        return Some((id, "permission to use, copy, modify"));
    }
    if has("zero-clause bsd") || has("0bsd") {
        return Some(("0BSD", "zero-clause bsd"));
    }
    if has("isc license") {
        return Some(("ISC", "isc license"));
    }
    if has("mit license") || has("mit licence") {
        return Some(("MIT", "mit license"));
    }
    if has("apache-2.0") || has("apache license") {
        return Some(("Apache-2.0", "apache license"));
    }
    None
}

/// The version number named within 250 characters after a licence title:
/// `version 2.0`, `version 3,`, `v. 2.0`, `v2`, `2.1`.
fn version_after(n: &str, pos: usize) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?:version|v\.?)\s*(\d(?:\.\d)?)|\b(\d\.\d)\b").expect("version regex")
    });
    let end = n
        .char_indices()
        .map(|(i, _)| i)
        .skip_while(|&i| i < pos)
        .nth(250)
        .unwrap_or(n.len());
    let window = &n[pos..end];
    let c = re.captures(window)?;
    c.get(1).or_else(|| c.get(2)).map(|m| m.as_str().to_owned())
}

/// Normalised id for comparisons: lower-case, no `-only`/`-or-later`,
/// spaces as dashes, `GPL-3.0` and `GPL-3` alike.
fn norm_id(id: &str) -> String {
    let mut s = id.trim().to_lowercase().replace(' ', "-");
    for suffix in ["-only", "-or-later", "-license", "-licence", "+"] {
        if let Some(stripped) = s.strip_suffix(suffix) {
            s = stripped.to_owned();
        }
    }
    s = s.replace("licence", "license");
    if let Some(stripped) = s.strip_suffix(".0") {
        s = stripped.to_owned();
    }
    s
}

/// The alternatives of a licence expression (`MIT OR Apache-2.0`,
/// `(MIT OR GPL-2.0)`, `MIT/Apache-2.0`).
fn alternatives(expr: &str) -> Vec<String> {
    let cleaned = expr.replace(['(', ')'], " ");
    let mut out = Vec::new();
    for part in cleaned.split('/') {
        for alt in or_regex().split(part) {
            let alt = alt.trim();
            if !alt.is_empty() {
                out.push(alt.to_owned());
            }
        }
    }
    out
}

/// Classify a licence text into an SPDX id with a confidence note.
pub fn classify(text: &str) -> (String, &'static str) {
    let n = normalise(text);
    let Some((id, title)) = identify(&n) else {
        if n.contains("all rights reserved") && n.len() < 2000 {
            return ("Proprietary".into(), "keywords");
        }
        return ("Unknown".into(), "keywords");
    };
    if spdx_tag_regex()
        .captures(&n)
        .is_some_and(|c| norm_id(&c[1]) == norm_id(id))
    {
        return (id.to_owned(), "exact");
    }
    let first_line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(normalise)
        .unwrap_or_default();
    if first_line.contains(title) {
        return (id.to_owned(), "exact");
    }
    let head: String = n.chars().take(400).collect();
    if head.contains(title) {
        return (id.to_owned(), "header");
    }
    (id.to_owned(), "keywords")
}

/// Copyleft (strong or network) licences that matter for redistribution.
pub fn is_copyleft(id: &str) -> bool {
    let l = id.to_uppercase();
    l.starts_with("GPL")
        || l.starts_with("AGPL")
        || l.starts_with("LGPL")
        || l.starts_with("SSPL")
        || l.starts_with("EUPL")
        || l.starts_with("CC-BY-SA")
        || l.starts_with("OSL")
}

/// A whole expression is copyleft when every alternative is.
fn expr_copyleft(expr: &str) -> bool {
    let alts = alternatives(expr);
    !alts.is_empty() && alts.iter().all(|a| is_copyleft(a))
}

fn expr_agpl(expr: &str) -> bool {
    let alts = alternatives(expr);
    !alts.is_empty() && alts.iter().all(|a| a.to_uppercase().starts_with("AGPL"))
}

fn is_permissive(id: &str) -> bool {
    let n = norm_id(id);
    n == "mit"
        || n.starts_with("apache")
        || n.starts_with("bsd")
        || n == "isc"
        || n == "0bsd"
        || n == "unlicense"
        || n == "zlib"
        || n == "bsl-1"
        || n.starts_with("cc0")
}

// ---------------------------------------------------------------------------
// Copyright lines and holders
// ---------------------------------------------------------------------------

fn or_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\s+or\s+").expect("or regex"))
}

fn present_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\b(present|now|current)\b").expect("present regex"))
}

fn email_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"<[^>]*@[^>]*>|\([^)]*@[^)]*\)").expect("email regex"))
}

fn spdx_tag_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"spdx-license-identifier:\s*([a-z0-9.+-]+)").expect("spdx tag regex")
    })
}

fn tag_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"<[A-Za-z/][^<>]{0,120}>").expect("tag regex"))
}

fn copyright_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\bcopyright\b|\bcopr\.|\(c\)|©").expect("copyright regex"))
}

fn year_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b(19[7-9]\d|20\d\d|2100)\b").expect("year regex"))
}

fn spdx_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)SPDX-License-Identifier:\s*([A-Za-z0-9.+\-]+(?:\s+(?:OR|AND|WITH)\s+[A-Za-z0-9.+\-]+)*)")
            .expect("spdx regex")
    })
}

/// Words after "copyright" that mean the line is prose, not an attribution.
const NOT_HOLDER_WORDS: &[&str] = &[
    "notice",
    "notices",
    "holder",
    "holders",
    "owner",
    "owners",
    "law",
    "and",
    "or",
    "of",
    "in",
    "to",
    "is",
    "for",
    "statement",
    "information",
    "header",
    "license",
    "licence",
    "year",
    "the above",
    "above",
    "interest",
    "treaty",
    "infringement",
    "protection",
    "terms",
    "on",
    "if",
    "a",
    "an",
    "that",
    "this",
    "which",
    "by the",
    "with",
    "symbol",
    "line",
    "lines",
];

/// Placeholders left in templates.
const PLACEHOLDERS: &[&str] = &[
    "{{",
    "${",
    "$",
    "<year>",
    "[year]",
    "yyyy",
    "<copyright",
    "<name",
    "<owner",
    "[name",
    "[fullname]",
    "<author",
    "[author",
    "<holder",
    "[copyright",
    "year>",
    "name of author",
    "copyright holders>",
    "your name",
    "insert ",
    "{year}",
    "{name}",
    "{author}",
];

/// Clean a copyright line into a holder name; `None` when it is prose, a
/// template placeholder, or too long to be a name.
fn normalize_holder(line: &str) -> Option<String> {
    // Markup around the notice (`<span>&copy;</span> 2022 <a>Name</a>`).
    let line = line.replace("&copy;", "©").replace("&#169;", "©");
    let line = tag_regex().replace_all(&line, " ");
    // The last marker wins: `Copyright (c) 2016`, `IDS_COPYRIGHT=Copyright 2016 X`.
    let m = copyright_regex().find_iter(&line).last()?;
    let rest = line[m.end()..].to_owned();
    let prefix = &line[..m.start()];
    let lower_rest = rest.trim().to_lowercase();
    // Prose: "copyright notice", "copyright law", ...
    if lower_rest.is_empty()
        || NOT_HOLDER_WORDS.iter().any(|w| {
            lower_rest == *w
                || lower_rest.starts_with(&format!("{w} "))
                || lower_rest.starts_with(&format!("{w},"))
        })
    {
        return None;
    }
    let had_year = year_regex().is_match(&rest);
    // Strip years, ranges, "present" and separators.
    let mut s = year_regex().replace_all(&rest, " ").into_owned();
    s = present_regex().replace_all(&s, " ").into_owned();
    s = s.replace(['-', '–', '—', ','], " ");
    // Trailing email.
    s = email_regex().replace_all(&s, " ").into_owned();
    // "All rights reserved" and anything after it.
    let lower = s.to_lowercase();
    if let Some(i) = lower.find("all rights reserved") {
        s.truncate(i);
    }
    // Comment closers and trailing punctuation.
    for closer in ["*/", "-->", "*)"] {
        if let Some(i) = s.find(closer) {
            s.truncate(i);
        }
    }
    let mut s = s
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    '.' | ','
                        | ';'
                        | ':'
                        | '-'
                        | '*'
                        | '/'
                        | '"'
                        | '\''
                        | '>'
                        | '<'
                        | '='
                        | '('
                        | ')'
                        | '|'
                        | '#'
                        | '!'
                        | '&'
                        | '\\'
                )
        })
        .to_owned();
    if let Some(stripped) = s.strip_prefix("by ").or_else(|| s.strip_prefix("By ")) {
        s = stripped.to_owned();
    }
    // Only a trailing "and others"/"et al." is still a name.
    if s.is_empty() || s.len() > 80 {
        return None;
    }
    let low = s.to_lowercase();
    if PLACEHOLDERS.iter().any(|p| low.contains(p)) || low.starts_with('<') || low.starts_with('[')
    {
        return None;
    }
    if !had_year && !lower_rest.starts_with("(c)") && !lower_rest.starts_with('©') {
        // A bare "Copyright Name" without a year or symbol: accept only when
        // the notice starts the comment and looks like a name, not a
        // sentence that happens to contain the word.
        let words: Vec<&str> = s.split(' ').collect();
        let clean_prefix = prefix.chars().all(|c| {
            c.is_whitespace()
                || matches!(
                    c,
                    '/' | '*'
                        | '#'
                        | ';'
                        | '%'
                        | '!'
                        | '-'
                        | '<'
                        | '>'
                        | '"'
                        | '\''
                        | '='
                        | '_'
                        | ':'
                        | '('
                        | ')'
                        | '@'
                )
        }) || prefix.trim_end().to_lowercase().ends_with("copyright");
        if !clean_prefix
            || words.len() > 6
            || !words[0].chars().next().is_some_and(|c| c.is_uppercase())
        {
            return None;
        }
    }
    if !s.chars().any(|c| c.is_alphabetic()) {
        return None;
    }
    Some(s)
}

/// Years mentioned on a copyright line, as (min, max).
fn years_of(line: &str) -> Option<(u32, u32)> {
    let mut min = u32::MAX;
    let mut max = 0u32;
    for m in year_regex().find_iter(line) {
        if let Ok(y) = m.as_str().parse::<u32>() {
            min = min.min(y);
            max = max.max(y);
        }
    }
    (max > 0).then_some((min, max))
}

fn merge_years(a: Option<(u32, u32)>, b: Option<(u32, u32)>) -> Option<(u32, u32)> {
    match (a, b) {
        (Some((a0, a1)), Some((b0, b1))) => Some((a0.min(b0), a1.max(b1))),
        (x, None) | (None, x) => x,
    }
}

/// Licence names people write in headers: `(phrase, label)`.
const HEADER_LICENSE_NAMES: &[(&str, &str)] = &[
    ("apache license, version 2.0", "Apache-2.0"),
    ("apache license version 2.0", "Apache-2.0"),
    ("apache license 2.0", "Apache-2.0"),
    ("apache-2.0", "Apache-2.0"),
    ("apache 2.0", "Apache-2.0"),
    ("mit license", "MIT"),
    ("mit licence", "MIT"),
    ("mit-licensed", "MIT"),
    ("gnu affero general public license", "AGPL"),
    ("gnu lesser general public license", "LGPL"),
    ("gnu library general public license", "LGPL"),
    ("gnu general public license", "GPL"),
    ("mozilla public license", "MPL-2.0"),
    ("bsd-style license", "BSD"),
    ("bsd style license", "BSD"),
    ("bsd license", "BSD"),
    ("bsd-3-clause", "BSD-3-Clause"),
    ("bsd-2-clause", "BSD-2-Clause"),
    ("isc license", "ISC"),
    ("boost software license", "BSL-1.0"),
    ("zlib license", "Zlib"),
    ("unlicense", "Unlicense"),
    ("public domain", "Public domain"),
    ("creative commons", "Creative Commons"),
    ("wtfpl", "WTFPL"),
];

/// What a file header says.
#[derive(Default)]
struct Header {
    /// `(holder, years)` per copyright line.
    holders: Vec<(String, Option<(u32, u32)>)>,
    spdx: Option<String>,
    licenses: Vec<String>,
}

fn parse_header(lines: &[&str]) -> Header {
    let mut h = Header::default();
    let mut seen: HashSet<String> = HashSet::new();
    for line in lines {
        if let Some(c) = spdx_regex().captures(line)
            && h.spdx.is_none()
        {
            h.spdx = Some(c[1].trim().to_owned());
        }
        if copyright_regex().is_match(line)
            && let Some(holder) = normalize_holder(line)
            && seen.insert(holder.to_lowercase())
        {
            h.holders.push((holder, years_of(line)));
        }
    }
    let joined = lines.join("\n").to_lowercase();
    for (phrase, label) in HEADER_LICENSE_NAMES {
        if joined.contains(phrase) && !h.licenses.iter().any(|l| l == label) {
            h.licenses.push((*label).to_owned());
        }
    }
    if let Some(s) = &h.spdx
        && !h.licenses.iter().any(|l| l == s)
    {
        h.licenses.insert(0, s.clone());
    }
    h
}

/// Read the first ~16 KB of a file; `None` when missing or binary.
fn read_head(workdir: &Path, file: &TrackedFile) -> Option<Vec<u8>> {
    let full = workdir.join(&file.path);
    let f = std::fs::File::open(&full).ok()?;
    let meta = f.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_TEXT_BYTES {
        return None;
    }
    let mut buf = Vec::with_capacity(16 * 1024);
    f.take(16 * 1024).read_to_end(&mut buf).ok()?;
    if crate::secrets::is_binary(&buf) {
        return None;
    }
    Some(buf)
}

// ---------------------------------------------------------------------------
// Project licence files and manifests
// ---------------------------------------------------------------------------

/// `LICENSE`, `LICENSE-MIT`, `LICENSE.txt`, `COPYING.LESSER`, `NOTICE.md`…
fn license_basename(base: &str) -> Option<&'static str> {
    // `license_checker.rs`, `License.java`: code, not a licence text.
    const CODE_EXTS: &[&str] = &[
        "rs", "py", "js", "ts", "tsx", "jsx", "mjs", "cjs", "go", "c", "cc", "cpp", "h", "hpp",
        "java", "kt", "kts", "rb", "php", "cs", "sh", "ps1", "bat", "cmd", "json", "yml", "yaml",
        "toml", "xml", "swift", "m", "mm", "dart", "scala", "pl", "lua", "sql", "gradle", "css",
        "scss", "vue", "svelte", "gn", "gni", "proto", "cmake", "mk", "ini", "cfg", "tpl", "tmpl",
        "hbs", "mustache", "ejs", "jinja", "j2", "erb", "haml", "pug", "jsp", "asp", "aspx",
        "razor", "cshtml", "vb", "fs", "ex", "exs", "erl", "hs", "ml", "clj", "lisp", "el", "vim",
        "zig", "nim", "d", "r", "jl", "tf", "hcl", "nix", "bzl", "bazel", "ninja", "in", "am",
        "ac", "sln", "csproj", "vcxproj", "pbxproj", "plist", "strings", "resx", "po", "pot",
    ];
    if CODE_EXTS.contains(&super::ext_of(base).as_str()) {
        return None;
    }
    let mut upper = base.to_uppercase();
    for ext in [".MD", ".TXT", ".RST", ".HTML", ".ADOC"] {
        if let Some(s) = upper.strip_suffix(ext) {
            upper = s.to_owned();
            break;
        }
    }
    let stem = upper.split(['-', '.', '_']).next().unwrap_or("");
    match stem {
        "LICENSE" | "LICENCE" | "COPYING" | "UNLICENSE" => Some("license"),
        "COPYRIGHT" | "NOTICE" => Some("notice"),
        _ => None,
    }
}

/// Licence texts anywhere in the tree (`common/licenses/CC-BY-4.0`,
/// `docs/COPYING`): not source headers.
fn is_license_text(path: &str) -> bool {
    let lower = path.to_lowercase();
    license_basename(basename(path)).is_some()
        || lower.contains("/licenses/")
        || lower.contains("/licences/")
        || lower.contains("/license/")
        || lower.contains("/licence/")
        || lower.starts_with("licenses/")
        || lower.starts_with("licences/")
}

/// Project-level licence files: at the root or in `LICENSES/`.
fn project_license_kind(path: &str) -> Option<&'static str> {
    let base = basename(path);
    if !path.contains('/') {
        return license_basename(base);
    }
    if let Some(rest) = path.strip_prefix("LICENSES/")
        && !rest.contains('/')
    {
        return Some(license_basename(base).unwrap_or("license"));
    }
    None
}

fn read_project_license(workdir: &Path, path: &str) -> Option<ProjectLicense> {
    let data = std::fs::read(workdir.join(path)).ok()?;
    if data.len() as u64 > MAX_TEXT_BYTES || crate::secrets::is_binary(&data) {
        return None;
    }
    let text = String::from_utf8_lossy(&data);
    let (id, confidence) = classify(&text);
    let mut holders = Vec::new();
    let mut seen = HashSet::new();
    let mut years = None;
    for line in text.lines() {
        if !copyright_regex().is_match(line) {
            continue;
        }
        if let Some(h) = normalize_holder(line) {
            years = merge_years(years, years_of(line));
            if seen.insert(h.to_lowercase()) {
                holders.push(h);
            }
        }
    }
    Some(ProjectLicense {
        path: path.to_owned(),
        id,
        confidence,
        holders,
        years,
    })
}

/// `license` of a `package.json`-like document: string, `{type}`, or
/// `licenses: [{type}]`.
fn json_license(v: &serde_json::Value) -> Option<String> {
    match v.get("license") {
        Some(serde_json::Value::String(s)) if !s.trim().is_empty() => {
            return Some(s.trim().to_owned());
        }
        Some(serde_json::Value::Object(o)) => {
            if let Some(serde_json::Value::String(s)) = o.get("type") {
                return Some(s.clone());
            }
        }
        Some(serde_json::Value::Array(a)) => {
            let names: Vec<String> = a.iter().filter_map(license_entry).collect();
            if !names.is_empty() {
                return Some(names.join(" OR "));
            }
        }
        _ => {}
    }
    if let Some(serde_json::Value::Array(a)) = v.get("licenses") {
        let names: Vec<String> = a.iter().filter_map(license_entry).collect();
        if !names.is_empty() {
            return Some(names.join(" OR "));
        }
    }
    None
}

fn license_entry(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Object(o) => o.get("type").and_then(|t| t.as_str()).map(str::to_owned),
        _ => None,
    }
}

/// `license = "…"` inside `[package]` of a Cargo.toml (or `[workspace.package]`).
fn cargo_toml_license(text: &str, section: &str) -> Option<String> {
    let header = format!("[{section}]");
    let start = text.find(&header)? + header.len();
    let body = &text[start..];
    let end = body.find("\n[").unwrap_or(body.len());
    let body = &body[..end];
    let re = Regex::new(r#"(?m)^\s*license\s*=\s*"([^"]+)""#).expect("cargo license regex");
    re.captures(body).map(|c| c[1].trim().to_owned())
}

fn cargo_toml_license_workspace(text: &str) -> bool {
    Regex::new(r"(?m)^\s*license\.workspace\s*=\s*true|^\s*license\s*=\s*\{\s*workspace\s*=\s*true")
        .expect("workspace regex")
        .is_match(text)
}

fn pyproject_license(text: &str) -> Option<String> {
    let re =
        Regex::new(r#"(?m)^\s*license\s*=\s*(?:"([^"]+)"|'([^']+)'|\{[^}]*text\s*=\s*"([^"]+)")"#)
            .expect("pyproject regex");
    let c = re.captures(text)?;
    c.get(1)
        .or_else(|| c.get(2))
        .or_else(|| c.get(3))
        .map(|m| m.as_str().trim().to_owned())
}

fn gemspec_license(text: &str) -> Option<String> {
    let re = Regex::new(r#"\.licenses?\s*=\s*\[?\s*["']([^"']+)["']"#).expect("gemspec regex");
    re.captures(text).map(|c| c[1].to_owned())
}

fn setup_py_license(text: &str) -> Option<String> {
    let re = Regex::new(r#"\blicense\s*=\s*["']([^"']+)["']"#).expect("setup regex");
    re.captures(text).map(|c| c[1].to_owned())
}

/// Under a directory of someone else's code.
fn is_vendored(path: &str) -> bool {
    let slashed = format!("/{path}");
    [
        "/node_modules/",
        "/vendor/",
        "/third_party/",
        "/third-party/",
    ]
    .iter()
    .any(|d| slashed.contains(d))
}

/// Directories (other than the root) that hold their own licence file: a
/// manifest there is not compared against the root LICENSE.
fn licensed_dirs(files: &[TrackedFile]) -> HashSet<String> {
    files
        .iter()
        .filter_map(|f| {
            let base = basename(&f.path);
            license_basename(base)?;
            let dir = &f.path[..f.path.len() - base.len()];
            let dir = dir.trim_end_matches('/');
            (!dir.is_empty()).then(|| dir.to_owned())
        })
        .collect()
}

/// `dir` or one of its ancestors (not the root) is in `dirs`.
fn under_licensed_dir(path: &str, dirs: &HashSet<String>) -> bool {
    let mut dir = match path.rfind('/') {
        Some(i) => &path[..i],
        None => return false,
    };
    loop {
        if dirs.contains(dir) {
            return true;
        }
        match dir.rfind('/') {
            Some(i) => dir = &dir[..i],
            None => return false,
        }
    }
}

/// Declared licences from the manifests (root-level ones plus any tracked
/// `Cargo.toml`/`package.json` outside node_modules, at most 50 reads).
fn declared_licenses(workdir: &Path, files: &[TrackedFile]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut reads = 0usize;
    let root_cargo = std::fs::read_to_string(workdir.join("Cargo.toml")).ok();
    let workspace_license = root_cargo
        .as_deref()
        .and_then(|t| cargo_toml_license(t, "workspace.package"));
    for f in files {
        if reads >= 50 {
            break;
        }
        let path = f.path.as_str();
        let base = basename(path);
        let root = !path.contains('/');
        if is_vendored(path) {
            continue;
        }
        let value = match base {
            "package.json" | "composer.json" => {
                reads += 1;
                std::fs::read_to_string(workdir.join(path))
                    .ok()
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                    .and_then(|v| json_license(&v))
            }
            "Cargo.toml" => {
                reads += 1;
                std::fs::read_to_string(workdir.join(path))
                    .ok()
                    .and_then(|t| {
                        cargo_toml_license(&t, "package").or_else(|| {
                            if cargo_toml_license_workspace(&t) {
                                workspace_license.clone()
                            } else {
                                None
                            }
                        })
                    })
            }
            "pyproject.toml" if root => {
                reads += 1;
                std::fs::read_to_string(workdir.join(path))
                    .ok()
                    .and_then(|t| pyproject_license(&t))
            }
            "setup.py" if root => {
                reads += 1;
                std::fs::read_to_string(workdir.join(path))
                    .ok()
                    .and_then(|t| setup_py_license(&t))
            }
            _ if base.ends_with(".gemspec") => {
                reads += 1;
                std::fs::read_to_string(workdir.join(path))
                    .ok()
                    .and_then(|t| gemspec_license(&t))
            }
            _ => None,
        };
        if let Some(v) = value {
            out.push((path.to_owned(), v));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Dependencies
// ---------------------------------------------------------------------------

/// Dependencies keyed by `(ecosystem, name, version)`; a later source only
/// fills in a licence the earlier one lacked.
#[derive(Default)]
struct Deps {
    map: BTreeMap<(&'static str, String, String), DepLicense>,
}

impl Deps {
    fn add(&mut self, dep: DepLicense) {
        let key = (dep.ecosystem, dep.name.clone(), dep.version.clone());
        match self.map.get_mut(&key) {
            Some(existing) => {
                if existing.license.is_none() && dep.license.is_some() {
                    *existing = dep;
                }
            }
            None => {
                self.map.insert(key, dep);
            }
        }
    }
}

/// `package-lock.json` v2/v3 `packages`, or v1 `dependencies` (no licence).
fn parse_package_lock(text: &str, deps: &mut Deps) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };
    if let Some(packages) = v.get("packages").and_then(|p| p.as_object()) {
        for (key, entry) in packages {
            let Some(idx) = key.rfind("node_modules/") else {
                continue;
            };
            let name = &key[idx + "node_modules/".len()..];
            if name.is_empty() {
                continue;
            }
            let version = entry
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            let license = json_license(entry);
            deps.add(DepLicense {
                name: name.to_owned(),
                version,
                ecosystem: "npm",
                source: if license.is_some() {
                    "package-lock.json".into()
                } else {
                    "package-lock.json (no licence field)".into()
                },
                license,
            });
        }
        return;
    }
    if let Some(dependencies) = v.get("dependencies").and_then(|d| d.as_object()) {
        for (name, entry) in dependencies {
            let version = entry
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            deps.add(DepLicense {
                name: name.clone(),
                version,
                ecosystem: "npm",
                license: None,
                source: "package-lock.json (no licence field)".into(),
            });
        }
    }
}

/// `node_modules/*/package.json` and `node_modules/@scope/*/package.json`.
fn scan_node_modules(workdir: &Path, deps: &mut Deps, cancel: &AtomicBool) {
    let dir = workdir.join("node_modules");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut reads = 0usize;
    let mut packages: Vec<PathBuf> = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if name.starts_with('@') {
            if let Ok(scoped) = std::fs::read_dir(e.path()) {
                for s in scoped.flatten() {
                    packages.push(s.path().join("package.json"));
                }
            }
        } else {
            packages.push(e.path().join("package.json"));
        }
    }
    packages.sort();
    for p in packages {
        if reads >= 5000 || cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        reads += 1;
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(name) = v.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        let version = v
            .get("version")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_owned();
        deps.add(DepLicense {
            name: name.to_owned(),
            version,
            ecosystem: "npm",
            license: json_license(&v),
            source: "node_modules".into(),
        });
    }
}

/// `registry/src/*` directories of the local cargo home.
fn cargo_registry_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|h| PathBuf::from(h).join(".cargo"))
        });
    let Some(home) = home else {
        return Vec::new();
    };
    std::fs::read_dir(home.join("registry").join("src"))
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default()
}

/// `[[package]]` entries of a Cargo.lock: `(name, version, has registry source)`.
fn parse_cargo_lock(text: &str) -> Vec<(String, String, bool)> {
    let mut out = Vec::new();
    let mut cur: Option<(String, String, bool)> = None;
    for line in text.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            if let Some(p) = cur.take()
                && !p.0.is_empty()
            {
                out.push(p);
            }
            cur = Some((String::new(), String::new(), false));
            continue;
        }
        let Some(p) = cur.as_mut() else {
            continue;
        };
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim().trim_matches('"');
            match k.trim() {
                "name" => p.0 = v.to_owned(),
                "version" => p.1 = v.to_owned(),
                "source" => p.2 = v.starts_with("registry+"),
                _ => {}
            }
        }
    }
    if let Some(p) = cur
        && !p.0.is_empty()
    {
        out.push(p);
    }
    out
}

/// Resolve a Cargo.lock's packages against a `cargo vendor` directory next
/// to it, then the local registry.
fn cargo_lock_deps(workdir: &Path, lock_path: &str, text: &str, deps: &mut Deps) {
    let lock_dir = match lock_path.rfind('/') {
        Some(i) => workdir.join(&lock_path[..i]),
        None => workdir.to_path_buf(),
    };
    let vendor = lock_dir.join("vendor");
    let mut sources: Vec<(PathBuf, &str)> = Vec::new();
    if vendor.is_dir() {
        sources.push((vendor, "vendored crate"));
    }
    for dir in cargo_registry_dirs() {
        sources.push((dir, "cargo registry"));
    }
    for (name, version, from_registry) in parse_cargo_lock(text) {
        if !from_registry {
            continue;
        }
        let mut license = None;
        let mut source = "Cargo.lock (registry copy not found)".to_owned();
        for (dir, label) in &sources {
            let manifest = dir.join(format!("{name}-{version}")).join("Cargo.toml");
            let manifest = if manifest.is_file() {
                manifest
            } else {
                dir.join(&name).join("Cargo.toml")
            };
            if let Ok(t) = std::fs::read_to_string(&manifest) {
                license = cargo_toml_license(&t, "package");
                source = if license.is_some() {
                    (*label).to_owned()
                } else {
                    format!("{label} (license-file only)")
                };
                break;
            }
        }
        deps.add(DepLicense {
            name,
            version,
            ecosystem: "cargo",
            license,
            source,
        });
    }
}

fn go_sum_deps(text: &str, deps: &mut Deps) {
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(module), Some(version)) = (parts.next(), parts.next()) else {
            continue;
        };
        let version = version.trim_end_matches("/go.mod");
        deps.add(DepLicense {
            name: module.to_owned(),
            version: version.to_owned(),
            ecosystem: "go",
            license: None,
            source: "go.sum (unknown)".into(),
        });
    }
}

/// `vendor/**/LICENSE*` and `third_party/*/LICENSE*`: the directory is the
/// dependency.
fn vendor_license_name(path: &str) -> Option<String> {
    let base = basename(path);
    if license_basename(base) != Some("license") {
        return None;
    }
    let dir = &path[..path.len() - base.len()];
    let dir = dir.trim_end_matches('/');
    for root in ["vendor/", "third_party/", "third-party/"] {
        if let Some(idx) = format!("/{dir}/").find(&format!("/{root}")) {
            let rel = &format!("/{dir}/")[idx + root.len() + 1..];
            let rel = rel.trim_matches('/');
            if !rel.is_empty() && (root == "vendor/" || !rel.contains('/')) {
                return Some(rel.to_owned());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Findings and the analysis
// ---------------------------------------------------------------------------

const WHY_NO_LICENSE: &str = "Without a licence the default of copyright law applies: nobody may copy, modify or redistribute the code, whatever the intent of publishing it. Add a LICENSE file (and the same id in the manifest).";
const WHY_MISMATCH: &str = "Package registries and tooling read the manifest, people read the LICENSE file; when they disagree, users cannot tell which terms apply and a dependency scanner may reject the package.";
const WHY_MULTIPLE: &str = "Several licence files without a note saying which applies to what leaves the terms ambiguous; state the dual licence or the per-directory split explicitly.";
const WHY_AGPL: &str = "The AGPL's network clause requires offering the source of the whole combined work to users who interact with it over a network; linking an AGPL dependency pulls a permissive project under those terms.";
const WHY_COPYLEFT: &str = "Distributing a permissive project with a copyleft dependency means the combined work must be released under the copyleft terms; check whether the dependency is linked, bundled or only used at build time.";
const WHY_UNKNOWN_DEPS: &str = "A dependency without a known licence cannot be cleared for redistribution; check the registry pages or run the ecosystem's licence tool.";
const WHY_FOREIGN: &str = "Files copied from other projects keep their copyright and licence; the headers must stay intact and the terms (attribution, notice files, copyleft) must be honoured in your releases.";
const WHY_STALE_YEAR: &str = "Purely informational: the copyright year in the LICENSE file lags the latest changes. Protection does not depend on it, but an outdated year looks unmaintained.";

/// Words too generic to tie two holders together.
const HOLDER_STOPWORDS: &[&str] = &[
    "authors",
    "author",
    "contributors",
    "software",
    "inc",
    "incorporated",
    "corp",
    "corporation",
    "ltd",
    "limited",
    "llc",
    "gmbh",
    "company",
    "foundation",
    "project",
    "team",
    "developers",
    "the",
    "and",
    "all",
    "rights",
    "reserved",
    "open",
    "source",
    "group",
    "labs",
    "technologies",
    "systems",
    "holdings",
    "international",
];

fn words_of(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| w.len() >= 4 && !HOLDER_STOPWORDS.contains(&w.as_str()))
        .collect()
}

fn current_year() -> u32 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }) as u32
}

/// Read LICENSE/COPYING files, manifests, dependency licences available
/// offline, and the headers of the tracked source files (first 40 lines of
/// each text file under 2 MB; parallel; `cancel` and progress per ~500
/// files). Never fails.
pub fn analyze(
    workdir: &Path,
    files: &[TrackedFile],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
    _commands: &mut Vec<String>,
) -> Licensing {
    let mut lic = Licensing::default();

    // Project licence files: LICENSE-like first, NOTICE/COPYRIGHT after.
    let mut license_paths: Vec<(&'static str, &str)> = files
        .iter()
        .filter_map(|f| project_license_kind(&f.path).map(|k| (k, f.path.as_str())))
        .collect();
    license_paths.sort_by(|a, b| a.0.cmp(b.0).reverse().then(a.1.cmp(b.1)));
    let mut has_license_file = false;
    for (kind, path) in &license_paths {
        if let Some(p) = read_project_license(workdir, path) {
            has_license_file |= *kind == "license";
            lic.project.push(p);
        }
    }
    let license_holders: Vec<String> = lic
        .project
        .iter()
        .flat_map(|p| p.holders.iter().cloned())
        .collect();

    lic.declared = declared_licenses(workdir, files);

    // Dependencies.
    let mut deps = Deps::default();
    for f in files {
        let path = f.path.as_str();
        if path.contains("node_modules/") {
            continue;
        }
        let base = basename(path);
        let root = !path.contains('/');
        match base {
            "package-lock.json" if root => {
                if let Ok(t) = std::fs::read_to_string(workdir.join(path)) {
                    parse_package_lock(&t, &mut deps);
                }
            }
            "Cargo.lock" => {
                if let Ok(t) = std::fs::read_to_string(workdir.join(path)) {
                    cargo_lock_deps(workdir, path, &t, &mut deps);
                }
            }
            "go.sum" if root => {
                if let Ok(t) = std::fs::read_to_string(workdir.join(path)) {
                    go_sum_deps(&t, &mut deps);
                }
            }
            _ => {
                if let Some(name) = vendor_license_name(path)
                    && let Some(p) = read_project_license(workdir, path)
                {
                    deps.add(DepLicense {
                        name,
                        version: String::new(),
                        ecosystem: "vendor",
                        license: (p.id != "Unknown").then_some(p.id),
                        source: "vendor LICENSE".into(),
                    });
                }
            }
        }
    }
    scan_node_modules(workdir, &mut deps, cancel);
    lic.dependencies = deps.map.into_values().collect();
    let mut summary: HashMap<String, u32> = HashMap::new();
    for d in &lic.dependencies {
        *summary
            .entry(d.license.clone().unwrap_or_else(|| "Unknown".into()))
            .or_default() += 1;
    }
    let mut summary: Vec<(String, u32)> = summary.into_iter().collect();
    summary.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    lic.dep_summary = summary;
    lic.copyleft = lic
        .dependencies
        .iter()
        .filter(|d| d.license.as_deref().is_some_and(expr_copyleft))
        .cloned()
        .collect();

    // Headers.
    let license_set: HashSet<&str> = license_paths.iter().map(|(_, p)| *p).collect();
    let candidates: Vec<&TrackedFile> = text_candidates(files)
        .into_iter()
        .filter(|f| {
            !license_set.contains(f.path.as_str())
                && vendor_license_name(&f.path).is_none()
                && !is_license_text(&f.path)
        })
        .collect();
    let (results, _visited) = scan_parallel(
        &candidates,
        "Reading licence headers",
        cancel,
        progress,
        &|| false,
        |file| {
            let data = read_head(workdir, file)?;
            let text = String::from_utf8_lossy(&data);
            let lines: Vec<&str> = text.lines().take(40).collect();
            let header = parse_header(&lines);
            Some(header)
        },
    );
    lic.files_scanned = results.len();
    let mut groups: BTreeMap<String, HolderGroup> = BTreeMap::new();
    let mut spdx: HashMap<String, usize> = HashMap::new();
    for (i, header) in &results {
        let path = &candidates[*i].path;
        if header.holders.is_empty() && header.spdx.is_none() {
            continue;
        }
        lic.files_with_headers += 1;
        if let Some(s) = &header.spdx {
            *spdx.entry(s.clone()).or_default() += 1;
        }
        for (holder, years) in &header.holders {
            let g = groups
                .entry(holder.to_lowercase())
                .or_insert_with(|| HolderGroup {
                    holder: holder.clone(),
                    ..HolderGroup::default()
                });
            g.files += 1;
            if g.examples.len() < 5 {
                g.examples.push(path.clone());
            }
            for l in &header.licenses {
                if !g.licenses.contains(l) {
                    g.licenses.push(l.clone());
                }
            }
            g.years = merge_years(g.years, *years);
        }
    }
    let mut holders: Vec<HolderGroup> = groups.into_values().collect();
    holders.sort_by(|a, b| b.files.cmp(&a.files).then(a.holder.cmp(&b.holder)));
    let mut spdx: Vec<(String, usize)> = spdx.into_iter().collect();
    spdx.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    lic.spdx_headers = spdx;

    // The project's own holders: LICENSE holders, the top header holder,
    // anything sharing a distinctive word with them or the directory name.
    let mut own_words: HashSet<String> = HashSet::new();
    let mut own: HashSet<String> = HashSet::new();
    for h in &license_holders {
        own.insert(h.to_lowercase());
        own_words.extend(words_of(h));
    }
    // The most frequent header holder is the project itself when nothing
    // else names it, or when it clearly dominates (half the headed files,
    // at least ten of them); a lone foreign header in a small repository
    // must not become "own" just because it is the only one.
    let headed: usize = holders.iter().map(|g| g.files).sum();
    if let Some(top) = holders.first()
        && (license_holders.is_empty() || (top.files >= 10 && top.files * 2 >= headed))
    {
        own.insert(top.holder.to_lowercase());
        own_words.extend(words_of(&top.holder));
    }
    if let Some(dir) = workdir.file_name().and_then(|n| n.to_str()) {
        own_words.extend(words_of(dir));
    }
    lic.foreign = holders
        .iter()
        .filter(|g| {
            !own.contains(&g.holder.to_lowercase())
                && !words_of(&g.holder).iter().any(|w| own_words.contains(w))
        })
        .cloned()
        .collect();
    lic.holders = holders;

    // Findings.
    let mut findings = Vec::new();
    let main_id = lic.main().map(str::to_owned);
    if !has_license_file && lic.declared.is_empty() {
        findings.push(LicenseFinding {
            severity: Severity::High,
            id: "no-license",
            title: "No licence".into(),
            detail: "The repository has no LICENSE, COPYING or UNLICENSE file and no manifest declares a licence, so by default nobody may legally copy, modify or redistribute the code.".into(),
            why: WHY_NO_LICENSE,
            paths: Vec::new(),
        });
    }
    let file_ids: Vec<String> = lic
        .project
        .iter()
        .filter(|p| p.id != "Unknown" && p.id != "Proprietary")
        .map(|p| norm_id(&p.id))
        .collect();
    if !file_ids.is_empty() {
        let own_dirs = licensed_dirs(files);
        let mismatched: Vec<&(String, String)> = lic
            .declared
            .iter()
            .filter(|(path, value)| {
                if under_licensed_dir(path, &own_dirs) {
                    return false;
                }
                let declared: Vec<String> =
                    alternatives(value).iter().map(|a| norm_id(a)).collect();
                !declared.is_empty()
                    && !declared.iter().any(|d| {
                        file_ids.iter().any(|f| {
                            f == d
                                || (d == "bsd" && f.starts_with("bsd"))
                                || (f == "bsd" && d.starts_with("bsd"))
                        })
                    })
            })
            .collect();
        if !mismatched.is_empty() {
            let file_names: Vec<String> = lic
                .project
                .iter()
                .filter(|p| p.id != "Unknown" && p.id != "Proprietary")
                .map(|p| format!("{} ({})", p.path, p.id))
                .collect();
            findings.push(LicenseFinding {
                severity: Severity::Medium,
                id: "license-mismatch",
                title: "Manifest and LICENSE file disagree".into(),
                detail: format!(
                    "{} declare{} {}, while the licence file{} {} {}.",
                    mismatched
                        .iter()
                        .map(|(p, v)| format!("{p} (`{v}`)"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    if mismatched.len() == 1 { "s" } else { "" },
                    if mismatched.len() == 1 {
                        "a different licence"
                    } else {
                        "different licences"
                    },
                    if file_names.len() == 1 { "" } else { "s" },
                    if file_names.len() == 1 { "is" } else { "are" },
                    file_names.join(", ")
                ),
                why: WHY_MISMATCH,
                paths: mismatched.iter().map(|(p, _)| p.clone()).collect(),
            });
        }
    }
    let distinct: BTreeSet<&str> = lic
        .project
        .iter()
        .filter(|p| p.id != "Unknown" && p.id != "Proprietary")
        .map(|p| p.id.as_str())
        .collect();
    let dual = distinct.len() == 2 && distinct.contains("MIT") && distinct.contains("Apache-2.0");
    if distinct.len() > 1 && !dual {
        findings.push(LicenseFinding {
            severity: Severity::Low,
            id: "multiple-licenses",
            title: "Several licence files with different terms".into(),
            detail: format!(
                "{}.",
                lic.project
                    .iter()
                    .filter(|p| distinct.contains(p.id.as_str()))
                    .map(|p| format!("{}: {}", p.path, p.id))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
            why: WHY_MULTIPLE,
            paths: lic.project.iter().map(|p| p.path.clone()).collect(),
        });
    }
    let project_agpl = main_id
        .as_deref()
        .is_some_and(|m| m.to_uppercase().starts_with("AGPL"));
    let agpl: Vec<&DepLicense> = lic
        .dependencies
        .iter()
        .filter(|d| d.license.as_deref().is_some_and(expr_agpl))
        .collect();
    if !agpl.is_empty() && !project_agpl {
        let names: Vec<String> = agpl
            .iter()
            .take(20)
            .map(|d| format!("{} {}", d.name, d.version).trim().to_owned())
            .collect();
        findings.push(LicenseFinding {
            severity: Severity::High,
            id: "agpl-dependency",
            title: format!(
                "{} AGPL dependenc{}",
                agpl.len(),
                if agpl.len() == 1 { "y" } else { "ies" }
            ),
            detail: format!(
                "{}{}",
                names.join(", "),
                if agpl.len() > names.len() {
                    format!(" and {} more", agpl.len() - names.len())
                } else {
                    String::new()
                }
            ),
            why: WHY_AGPL,
            paths: Vec::new(),
        });
    }
    let permissive = main_id.as_deref().is_some_and(is_permissive);
    let copyleft: Vec<&DepLicense> = lic
        .copyleft
        .iter()
        .filter(|d| !d.license.as_deref().is_some_and(expr_agpl))
        .collect();
    if permissive && !copyleft.is_empty() {
        let names: Vec<String> = copyleft
            .iter()
            .take(20)
            .map(|d| {
                format!(
                    "{} {} ({})",
                    d.name,
                    d.version,
                    d.license.clone().unwrap_or_default()
                )
                .replace("  ", " ")
            })
            .collect();
        findings.push(LicenseFinding {
            severity: Severity::Medium,
            id: "copyleft-dependency",
            title: format!(
                "{} copyleft dependenc{} in a {} project",
                copyleft.len(),
                if copyleft.len() == 1 { "y" } else { "ies" },
                main_id.clone().unwrap_or_default()
            ),
            detail: format!(
                "{}{}",
                names.join(", "),
                if copyleft.len() > names.len() {
                    format!(" and {} more", copyleft.len() - names.len())
                } else {
                    String::new()
                }
            ),
            why: WHY_COPYLEFT,
            paths: Vec::new(),
        });
    }
    let knowable: Vec<&DepLicense> = lic
        .dependencies
        .iter()
        .filter(|d| !d.source.starts_with("go.sum"))
        .collect();
    let unknown = knowable.iter().filter(|d| d.license.is_none()).count();
    if !knowable.is_empty() && unknown * 10 > knowable.len() {
        let examples: Vec<String> = knowable
            .iter()
            .filter(|d| d.license.is_none())
            .take(10)
            .map(|d| d.name.clone())
            .collect();
        findings.push(LicenseFinding {
            severity: Severity::Low,
            id: "unknown-dependency-licenses",
            title: format!(
                "{unknown} of {} dependencies have no known licence",
                knowable.len()
            ),
            detail: format!(
                "No licence could be read offline for {}{}.",
                examples.join(", "),
                if unknown > examples.len() {
                    format!(" and {} more", unknown - examples.len())
                } else {
                    String::new()
                }
            ),
            why: WHY_UNKNOWN_DEPS,
            paths: Vec::new(),
        });
    }
    if !lic.foreign.is_empty() {
        let total_files: usize = lic.foreign.iter().map(|g| g.files).sum();
        let top: Vec<String> = lic
            .foreign
            .iter()
            .take(5)
            .map(|g| {
                format!(
                    "{} ({} file{}{}; e.g. {})",
                    g.holder,
                    g.files,
                    if g.files == 1 { "" } else { "s" },
                    if g.licenses.is_empty() {
                        String::new()
                    } else {
                        format!(", {}", g.licenses.join("/"))
                    },
                    g.examples.first().cloned().unwrap_or_default()
                )
            })
            .collect();
        findings.push(LicenseFinding {
            severity: Severity::Low,
            id: "foreign-copyright",
            title: format!(
                "{} file{} other projects' copyright",
                total_files,
                if total_files == 1 {
                    " carries"
                } else {
                    "s carry"
                }
            ),
            detail: format!(
                "{}{}. Copied code keeps its licence terms.",
                top.join("; "),
                if lic.foreign.len() > top.len() {
                    format!("; and {} more holders", lic.foreign.len() - top.len())
                } else {
                    String::new()
                }
            ),
            why: WHY_FOREIGN,
            paths: lic
                .foreign
                .iter()
                .flat_map(|g| g.examples.iter().take(2).cloned())
                .take(10)
                .collect(),
        });
    }
    let year = current_year();
    let stale: Vec<&ProjectLicense> = lic
        .project
        .iter()
        .filter(|p| p.years.is_some_and(|(_, max)| max + 2 < year))
        .collect();
    if !stale.is_empty() {
        findings.push(LicenseFinding {
            severity: Severity::Low,
            id: "stale-copyright-year",
            title: "Copyright year in the licence file is out of date".into(),
            detail: format!(
                "{}.",
                stale
                    .iter()
                    .map(|p| format!("{} says {}", p.path, p.years.map(|(_, m)| m).unwrap_or(0)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            why: WHY_STALE_YEAR,
            paths: stale.iter().map(|p| p.path.clone()).collect(),
        });
    }
    findings.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.id.cmp(b.id)));
    lic.findings = findings;
    lic
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIT: &str = "MIT License\n\nCopyright (c) 2019-2024 Example Authors\n\nPermission is hereby granted, free of charge, to any person obtaining a copy\nof this software and associated documentation files (the \"Software\"), to deal\nin the Software without restriction...\n\nTHE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR\nIMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,\n";
    const APACHE: &str = "                                 Apache License\n                           Version 2.0, January 2004\n                        http://www.apache.org/licenses/\n\n   TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION\n";
    const GPL3: &str = "                    GNU GENERAL PUBLIC LICENSE\n                       Version 3, 29 June 2007\n\n Copyright (C) 2007 Free Software Foundation, Inc. <https://fsf.org/>\n Everyone is permitted to copy and distribute verbatim copies\n of this license document, but changing it is not allowed.\n";
    const BSD3: &str = "Copyright (c) 2020, Someone\nAll rights reserved.\n\nRedistribution and use in source and binary forms, with or without\nmodification, are permitted provided that the following conditions are met:\n\n1. Redistributions of source code must retain the above copyright notice...\n3. Neither the name of the copyright holder nor the names of its\n   contributors may be used to endorse or promote products derived from\n   this software without specific prior written permission.\n";

    #[test]
    fn classifies_common_licences() {
        assert_eq!(classify(MIT), ("MIT".to_owned(), "exact"));
        assert_eq!(classify(APACHE), ("Apache-2.0".to_owned(), "exact"));
        assert_eq!(classify(GPL3), ("GPL-3.0-only".to_owned(), "exact"));
        assert_eq!(classify(BSD3), ("BSD-3-Clause".to_owned(), "header"));
        assert_eq!(
            classify("This project has no terms.\n"),
            ("Unknown".to_owned(), "keywords")
        );
        assert_eq!(
            classify("Copyright 2024 Acme. All rights reserved.\n"),
            ("Proprietary".to_owned(), "keywords")
        );
        assert_eq!(
            classify("// SPDX-License-Identifier: Apache-2.0\n// Apache License 2.0").0,
            "Apache-2.0"
        );
        assert_eq!(
            classify("GNU LESSER GENERAL PUBLIC LICENSE\nVersion 2.1, February 1999").0,
            "LGPL-2.1-only"
        );
        assert_eq!(
            classify("GNU AFFERO GENERAL PUBLIC LICENSE\nVersion 3, 19 November 2007").0,
            "AGPL-3.0-only"
        );
        assert_eq!(
            classify("Mozilla Public License Version 2.0\n==================================").0,
            "MPL-2.0"
        );
        assert_eq!(
            classify("This is free and unencumbered software released into the public domain.").0,
            "Unlicense"
        );
        assert_eq!(
            classify("ISC License\n\nPermission to use, copy, modify, and/or distribute this software for any purpose with or without fee is hereby granted, provided that the above copyright notice and this permission notice appear in all copies.").0,
            "ISC"
        );
        assert_eq!(
            classify("Permission to use, copy, modify, and/or distribute this software for any purpose with or without fee is hereby granted.\n\nTHE SOFTWARE IS PROVIDED \"AS IS\"").0,
            "0BSD"
        );
        assert_eq!(classify("Boost Software License - Version 1.0 - August 17th, 2003\n\nPermission is hereby granted, free of charge...").0, "BSL-1.0");
    }

    #[test]
    fn licences_that_quote_other_licences() {
        // MPL-2.0 names the GPL, LGPL and AGPL as "Secondary Licenses".
        let mpl = "Mozilla Public License Version 2.0\n==================================\n\n1. Definitions\n\n1.12. \"Secondary License\"\n    means either the GNU General Public License, Version 2.0, the GNU Lesser General Public License, Version 2.1, the GNU Affero General Public License, Version 3.0, or any later versions of those licenses.\n";
        assert_eq!(classify(mpl), ("MPL-2.0".to_owned(), "exact"));
        // GPL-3 §13 names the AGPL.
        let gpl = "GNU GENERAL PUBLIC LICENSE\nVersion 3, 29 June 2007\n\n13. Use with the GNU Affero General Public License.\n\nNotwithstanding any other provision of this License, you have permission to link or combine any covered work with a work licensed under version 3 of the GNU Affero General Public License into a single combined work.\n";
        assert_eq!(classify(gpl), ("GPL-3.0-only".to_owned(), "exact"));
        // LGPL-2.1 incorporates the GPL and names its version 2.
        let lgpl = "GNU LESSER GENERAL PUBLIC LICENSE\nVersion 2.1, February 1999\n\nThis license is the ordinary GNU General Public License, version 2, modified...\n";
        assert_eq!(classify(lgpl).0, "LGPL-2.1-only");
        // Short header blurbs.
        assert_eq!(classify("This program is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation, either version 2 of the License, or (at your option) any later version.").0, "GPL-2.0-only");
        assert_eq!(
            classify("Licensed under the EUPL, Version 1.2 (European Union Public Licence)\n").0,
            "EUPL-1.2"
        );
        assert_eq!(
            version_after("mozilla public license, v. 2.0", 0).as_deref(),
            Some("2.0")
        );
        assert_eq!(
            version_after("gnu general public license version 3, 29 june 2007", 0).as_deref(),
            Some("3")
        );
    }

    #[test]
    fn holders_from_messy_lines() {
        assert_eq!(
            normalize_holder("IDS_COPYRIGHT=Copyright 2016 The Brave Authors"),
            Some("The Brave Authors".into())
        );
        assert_eq!(
            normalize_holder(
                "<span>&copy;</span> 2022 <a href=\"/\">Site News Ltd</a> or its affiliates"
            ),
            Some("Site News Ltd or its affiliates".into())
        );
        assert_eq!(
            normalize_holder(
                "More considerations for licensors: copyright and certain other rights"
            ),
            None
        );
        assert_eq!(
            normalize_holder("    the copyright holder's permission is required"),
            None
        );
        assert_eq!(
            normalize_holder("// Copyright Google LLC"),
            Some("Google LLC".into())
        );
        assert!(is_license_text("common/licenses/CC-BY-4.0"));
        assert!(is_license_text("docs/COPYING"));
        assert!(!is_license_text("src/license_checker.rs"));
        assert_eq!(license_basename("License.java"), None);
        assert!(is_vendored("third_party/rust/vendor/x/Cargo.toml"));
        assert!(!is_vendored("crates/core/Cargo.toml"));
        let dirs: HashSet<String> = ["crates/other".to_owned()].into_iter().collect();
        assert!(under_licensed_dir("crates/other/sub/Cargo.toml", &dirs));
        assert!(!under_licensed_dir("crates/core/Cargo.toml", &dirs));
        assert!(!under_licensed_dir("Cargo.toml", &dirs));
    }

    #[test]
    fn normalises_holders() {
        assert_eq!(
            normalize_holder("// Copyright (c) 2019-2024 Other Corp. All rights reserved."),
            Some("Other Corp".into())
        );
        assert_eq!(
            normalize_holder("# Copyright 2024 The Chromium Authors"),
            Some("The Chromium Authors".into())
        );
        assert_eq!(
            normalize_holder(" * Copyright © 2001, 2002 Jane Doe <jane@example.com>"),
            Some("Jane Doe".into())
        );
        assert_eq!(
            normalize_holder("Copyright (c) <year> <copyright holders>"),
            None
        );
        assert_eq!(normalize_holder("Copyright {{ year }} {{ author }}"), None);
        assert_eq!(normalize_holder("Copyright $YEAR $NAME"), None);
        assert_eq!(
            normalize_holder("The above copyright notice shall be included"),
            None
        );
        assert_eq!(
            normalize_holder("subject to copyright law in some countries"),
            None
        );
        assert_eq!(
            normalize_holder("Copyright 2015-present Facebook, Inc."),
            Some("Facebook Inc".into())
        );
        assert_eq!(years_of("Copyright (c) 2019-2024 X"), Some((2019, 2024)));
        assert_eq!(years_of("Copyright X"), None);
    }

    #[test]
    fn parses_headers() {
        let h = parse_header(&[
            "// Copyright (c) 2021 Brave Software, Inc.",
            "// SPDX-License-Identifier: MPL-2.0",
            "// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0.",
            "fn main() {}",
        ]);
        assert_eq!(h.holders.len(), 1);
        assert_eq!(h.holders[0].0, "Brave Software Inc");
        assert_eq!(h.spdx.as_deref(), Some("MPL-2.0"));
        assert_eq!(h.licenses, ["MPL-2.0"]);
        let empty = parse_header(&["fn main() {}", "// nothing here"]);
        assert!(empty.holders.is_empty() && empty.spdx.is_none());
    }

    #[test]
    fn parses_package_lock_v3() {
        let lock = r#"{
  "name": "x", "lockfileVersion": 3,
  "packages": {
    "": {"name": "x", "license": "ISC"},
    "node_modules/left-pad": {"version": "1.3.0", "license": "WTFPL"},
    "node_modules/@scope/pkg": {"version": "2.0.0", "license": {"type": "MIT"}},
    "node_modules/a/node_modules/nested": {"version": "0.1.0"}
  }
}"#;
        let mut deps = Deps::default();
        parse_package_lock(lock, &mut deps);
        let all: Vec<&DepLicense> = deps.map.values().collect();
        assert_eq!(all.len(), 3);
        let left = all.iter().find(|d| d.name == "left-pad").unwrap();
        assert_eq!(left.version, "1.3.0");
        assert_eq!(left.license.as_deref(), Some("WTFPL"));
        assert_eq!(left.source, "package-lock.json");
        let scoped = all.iter().find(|d| d.name == "@scope/pkg").unwrap();
        assert_eq!(scoped.license.as_deref(), Some("MIT"));
        let nested = all.iter().find(|d| d.name == "nested").unwrap();
        assert_eq!(nested.license, None);
        assert_eq!(nested.source, "package-lock.json (no licence field)");

        let v1 = r#"{"lockfileVersion": 1, "dependencies": {"foo": {"version": "1.0.0"}}}"#;
        let mut deps = Deps::default();
        parse_package_lock(v1, &mut deps);
        assert_eq!(deps.map.len(), 1);
        assert!(deps.map.values().all(|d| d.license.is_none()));
    }

    #[test]
    fn manifests_and_lockfiles() {
        assert_eq!(
            cargo_toml_license(
                "[package]\nname = \"x\"\nlicense = \"MIT OR Apache-2.0\"\n\n[dependencies]\n",
                "package"
            )
            .as_deref(),
            Some("MIT OR Apache-2.0")
        );
        assert!(cargo_toml_license_workspace(
            "[package]\nlicense.workspace = true\n"
        ));
        assert_eq!(
            pyproject_license("[project]\nlicense = {text = \"BSD-3-Clause\"}\n").as_deref(),
            Some("BSD-3-Clause")
        );
        assert_eq!(
            pyproject_license("[project]\nlicense = \"MIT\"\n").as_deref(),
            Some("MIT")
        );
        assert_eq!(
            gemspec_license("  s.licenses = ['MIT']\n").as_deref(),
            Some("MIT")
        );
        assert_eq!(
            setup_py_license("setup(name='x', license='Apache-2.0')").as_deref(),
            Some("Apache-2.0")
        );
        let lock = parse_cargo_lock(
            "[[package]]\nname = \"a\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \"local\"\nversion = \"0.1.0\"\n",
        );
        assert_eq!(
            lock,
            vec![
                ("a".to_owned(), "1.0.0".to_owned(), true),
                ("local".to_owned(), "0.1.0".to_owned(), false)
            ]
        );
        assert_eq!(
            vendor_license_name("vendor/github.com/foo/bar/LICENSE").as_deref(),
            Some("github.com/foo/bar")
        );
        assert_eq!(
            vendor_license_name("third_party/zlib/LICENSE.txt").as_deref(),
            Some("zlib")
        );
        assert_eq!(vendor_license_name("third_party/zlib/sub/LICENSE"), None);
        assert_eq!(vendor_license_name("src/LICENSE"), None);
        assert_eq!(project_license_kind("LICENSE-MIT"), Some("license"));
        assert_eq!(project_license_kind("COPYING.LESSER"), Some("license"));
        assert_eq!(project_license_kind("NOTICE.txt"), Some("notice"));
        assert_eq!(project_license_kind("LICENSES/MIT.txt"), Some("license"));
        assert_eq!(project_license_kind("docs/LICENSE"), None);
    }

    #[test]
    fn expressions_and_ids() {
        assert_eq!(norm_id("GPL-3.0-only"), "gpl-3");
        assert_eq!(norm_id("GPL-3.0"), "gpl-3");
        assert_eq!(norm_id("Apache License 2.0"), "apache-license-2");
        assert_eq!(alternatives("(MIT OR Apache-2.0)"), ["MIT", "Apache-2.0"]);
        assert_eq!(alternatives("MIT/X11"), ["MIT", "X11"]);
        assert!(expr_copyleft("GPL-2.0-or-later"));
        assert!(!expr_copyleft("GPL-2.0 OR MIT"));
        assert!(expr_agpl("AGPL-3.0"));
        assert!(!expr_agpl("MIT"));
        assert!(
            is_permissive("Apache-2.0")
                && is_permissive("BSD-2-Clause")
                && !is_permissive("GPL-3.0-only")
        );
        assert!(current_year() >= 2026);
    }
}
