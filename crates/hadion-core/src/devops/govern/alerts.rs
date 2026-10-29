//! Dependabot and code-scanning alerts: models, reads, the dismiss / reopen
//! request builders, fix guidance and the matching Dependabot pull request.

use std::path::Path;

use serde::Deserialize;

use crate::Result;
use crate::devops::gh::{Gh, Request};
use crate::github::{ApiCall, PullRequest, parse_time};
use crate::osint::Package;

/// The longest dismissal comment GitHub accepts.
pub const MAX_COMMENT: usize = 280;

/// Alert severity, most severe first in `Ord` order reversed (`Critical`
/// sorts greatest).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Severity {
    #[default]
    Unknown,
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    pub const ALL: [Severity; 4] = [
        Severity::Critical,
        Severity::High,
        Severity::Medium,
        Severity::Low,
    ];

    /// From GitHub's `critical` / `high` / `moderate` / `medium` / `low`, or
    /// code scanning's `error` / `warning` / `note`.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "critical" => Severity::Critical,
            "high" | "error" => Severity::High,
            "medium" | "moderate" | "warning" => Severity::Medium,
            "low" | "note" | "negligible" => Severity::Low,
            _ => Severity::Unknown,
        }
    }

    /// From a CVSS score.
    pub fn from_score(score: f64) -> Self {
        if score >= 9.0 {
            Severity::Critical
        } else if score >= 7.0 {
            Severity::High
        } else if score >= 4.0 {
            Severity::Medium
        } else if score > 0.0 {
            Severity::Low
        } else {
            Severity::Unknown
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Severity::Critical => "Critical",
            Severity::High => "High",
            Severity::Medium => "Medium",
            Severity::Low => "Low",
            Severity::Unknown => "Unknown",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Severity::Critical => 0,
            Severity::High => 1,
            Severity::Medium => 2,
            Severity::Low => 3,
            Severity::Unknown => 4,
        }
    }
}

/// Counts per severity: critical, high, medium, low, unknown.
pub fn severity_counts(items: impl Iterator<Item = Severity>) -> [usize; 5] {
    let mut c = [0usize; 5];
    for s in items {
        c[s.index()] += 1;
    }
    c
}

/// Which alerts a list shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum StateFilter {
    #[default]
    Open,
    Dismissed,
    Fixed,
    All,
}

impl StateFilter {
    pub const ALL: [StateFilter; 4] = [
        StateFilter::Open,
        StateFilter::Dismissed,
        StateFilter::Fixed,
        StateFilter::All,
    ];

    pub fn label(self) -> &'static str {
        match self {
            StateFilter::Open => "Open",
            StateFilter::Dismissed => "Dismissed",
            StateFilter::Fixed => "Fixed",
            StateFilter::All => "All",
        }
    }

    /// The `state=` query value (`None` for all).
    fn dependabot_query(self) -> Option<&'static str> {
        match self {
            StateFilter::Open => Some("open"),
            StateFilter::Dismissed => Some("dismissed,auto_dismissed"),
            StateFilter::Fixed => Some("fixed"),
            StateFilter::All => None,
        }
    }

    fn code_query(self) -> Option<&'static str> {
        match self {
            StateFilter::Open => Some("open"),
            StateFilter::Dismissed => Some("dismissed"),
            StateFilter::Fixed => Some("fixed"),
            StateFilter::All => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Dependabot
// ---------------------------------------------------------------------------

/// Why a Dependabot alert is dismissed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DependabotReason {
    FixStarted,
    Inaccurate,
    NoBandwidth,
    NotUsed,
    #[default]
    TolerableRisk,
}

impl DependabotReason {
    pub const ALL: [DependabotReason; 5] = [
        DependabotReason::FixStarted,
        DependabotReason::Inaccurate,
        DependabotReason::NoBandwidth,
        DependabotReason::NotUsed,
        DependabotReason::TolerableRisk,
    ];

    /// The API value.
    pub fn api(self) -> &'static str {
        match self {
            DependabotReason::FixStarted => "fix_started",
            DependabotReason::Inaccurate => "inaccurate",
            DependabotReason::NoBandwidth => "no_bandwidth",
            DependabotReason::NotUsed => "not_used",
            DependabotReason::TolerableRisk => "tolerable_risk",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DependabotReason::FixStarted => "A fix has already been started",
            DependabotReason::Inaccurate => "This alert is inaccurate or incorrect",
            DependabotReason::NoBandwidth => "No bandwidth to fix this",
            DependabotReason::NotUsed => "Vulnerable code is not actually used",
            DependabotReason::TolerableRisk => "Risk is tolerable to this project",
        }
    }
}

/// One Dependabot alert.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DependabotAlert {
    pub number: u64,
    /// `open`, `dismissed`, `fixed`, `auto_dismissed`.
    pub state: String,
    pub severity: Severity,
    pub package: String,
    /// GitHub's ecosystem name: `npm`, `pip`, `rust`, `go`, `rubygems`, …
    pub ecosystem: String,
    pub manifest: String,
    /// `runtime` or `development`.
    pub scope: String,
    /// `direct`, `transitive` or `inconclusive`.
    pub relationship: String,
    pub vulnerable_range: String,
    pub first_patched: Option<String>,
    pub ghsa: String,
    pub cve: Option<String>,
    pub summary: String,
    pub description: String,
    pub references: Vec<String>,
    pub cvss_score: Option<f64>,
    pub cvss_vector: Option<String>,
    /// `(CWE-79, Cross-site scripting)`.
    pub cwes: Vec<(String, String)>,
    /// EPSS `(percentage, percentile)`, both 0..1.
    pub epss: Option<(f64, f64)>,
    pub published: Option<i64>,
    pub created: Option<i64>,
    pub updated: Option<i64>,
    pub dismissed_at: Option<i64>,
    pub dismissed_by: Option<String>,
    pub dismissed_reason: Option<String>,
    pub dismissed_comment: Option<String>,
    pub fixed_at: Option<i64>,
    pub html_url: String,
}

impl DependabotAlert {
    pub fn is_open(&self) -> bool {
        self.state == "open"
    }

    /// `CVE-… · GHSA-…`.
    pub fn ids(&self) -> String {
        match &self.cve {
            Some(c) => format!("{c} · {}", self.ghsa),
            None => self.ghsa.clone(),
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawPkg {
    ecosystem: String,
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawDependency {
    package: Option<RawPkg>,
    manifest_path: String,
    scope: Option<String>,
    relationship: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawPatched {
    identifier: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawVulnerability {
    package: Option<RawPkg>,
    severity: String,
    vulnerable_version_range: String,
    first_patched_version: Option<RawPatched>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawCvss {
    vector_string: Option<String>,
    score: Option<f64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawCwe {
    cwe_id: String,
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawEpss {
    percentage: Option<f64>,
    percentile: Option<f64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawReference {
    url: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawAdvisory {
    ghsa_id: String,
    cve_id: Option<String>,
    summary: String,
    description: String,
    severity: String,
    cvss: Option<RawCvss>,
    cwes: Vec<RawCwe>,
    epss: Option<RawEpss>,
    references: Vec<RawReference>,
    published_at: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawUser {
    login: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawDependabotAlert {
    number: u64,
    state: String,
    dependency: Option<RawDependency>,
    security_advisory: Option<RawAdvisory>,
    security_vulnerability: Option<RawVulnerability>,
    html_url: String,
    created_at: Option<String>,
    updated_at: Option<String>,
    dismissed_at: Option<String>,
    dismissed_by: Option<RawUser>,
    dismissed_reason: Option<String>,
    dismissed_comment: Option<String>,
    fixed_at: Option<String>,
}

fn time(s: &Option<String>) -> Option<i64> {
    s.as_deref().and_then(parse_time)
}

fn dependabot_from_raw(a: RawDependabotAlert) -> DependabotAlert {
    let dep = a.dependency.unwrap_or_default();
    let vuln = a.security_vulnerability.unwrap_or_default();
    let pkg = dep.package.or(vuln.package).unwrap_or_default();
    let adv = a.security_advisory.unwrap_or_default();
    let cvss = adv.cvss.unwrap_or_default();
    let sev_text = if vuln.severity.is_empty() {
        adv.severity.clone()
    } else {
        vuln.severity.clone()
    };
    DependabotAlert {
        number: a.number,
        state: a.state,
        severity: Severity::parse(&sev_text),
        package: pkg.name,
        ecosystem: pkg.ecosystem,
        manifest: dep.manifest_path,
        scope: dep.scope.unwrap_or_default(),
        relationship: dep.relationship.unwrap_or_default(),
        vulnerable_range: vuln.vulnerable_version_range,
        first_patched: vuln
            .first_patched_version
            .map(|p| p.identifier)
            .filter(|s| !s.is_empty()),
        ghsa: adv.ghsa_id,
        cve: adv.cve_id.filter(|s| !s.is_empty()),
        summary: adv.summary,
        description: adv.description,
        references: adv
            .references
            .into_iter()
            .map(|r| r.url)
            .filter(|u| !u.is_empty())
            .collect(),
        cvss_score: cvss.score.filter(|s| *s > 0.0),
        cvss_vector: cvss.vector_string.filter(|s| !s.is_empty()),
        cwes: adv.cwes.into_iter().map(|c| (c.cwe_id, c.name)).collect(),
        epss: adv
            .epss
            .and_then(|e| Some((e.percentage?, e.percentile.unwrap_or(0.0)))),
        published: time(&adv.published_at),
        created: time(&a.created_at),
        updated: time(&a.updated_at),
        dismissed_at: time(&a.dismissed_at),
        dismissed_by: a.dismissed_by.map(|u| u.login).filter(|s| !s.is_empty()),
        dismissed_reason: a.dismissed_reason,
        dismissed_comment: a.dismissed_comment.filter(|s| !s.is_empty()),
        fixed_at: time(&a.fixed_at),
        html_url: a.html_url,
    }
}

/// Decode a page of `GET dependabot/alerts`.
pub fn parse_dependabot(body: &[u8]) -> Result<Vec<DependabotAlert>> {
    let raw: Vec<RawDependabotAlert> = crate::devops::gh::decode(body, "Dependabot alerts")?;
    let mut v: Vec<DependabotAlert> = raw.into_iter().map(dependabot_from_raw).collect();
    sort_dependabot(&mut v);
    Ok(v)
}

/// Most severe first, then newest.
pub fn sort_dependabot(v: &mut [DependabotAlert]) {
    v.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then(b.created.cmp(&a.created))
            .then(b.number.cmp(&a.number))
    });
}

/// `dependabot/alerts?state=…&per_page=100` for a filter.
pub fn dependabot_endpoint(gh: &Gh, filter: StateFilter) -> String {
    match filter.dependabot_query() {
        Some(s) => gh.path(&format!("dependabot/alerts?state={s}&per_page=100")),
        None => gh.path("dependabot/alerts?per_page=100"),
    }
}

/// The first 100 alerts in a state (one request).
pub fn fetch_dependabot(
    gh: &Gh,
    filter: StateFilter,
    calls: &mut Vec<ApiCall>,
) -> Result<Vec<DependabotAlert>> {
    parse_dependabot(&gh.get(&dependabot_endpoint(gh, filter), calls)?)
}

fn comment_value(comment: &str) -> Option<String> {
    let c = comment.trim();
    if c.is_empty() {
        None
    } else {
        Some(c.chars().take(MAX_COMMENT).collect())
    }
}

/// `PATCH dependabot/alerts/{n}` dismissing an alert.
pub fn dependabot_dismiss(
    gh: &Gh,
    number: u64,
    reason: DependabotReason,
    comment: &str,
) -> Request {
    let mut body = serde_json::json!({
        "state": "dismissed",
        "dismissed_reason": reason.api(),
    });
    if let Some(c) = comment_value(comment) {
        body["dismissed_comment"] = serde_json::Value::String(c);
    }
    Request::patch(gh.path(&format!("dependabot/alerts/{number}")), body)
}

/// `PATCH dependabot/alerts/{n}` reopening a dismissed alert.
pub fn dependabot_reopen(gh: &Gh, number: u64) -> Request {
    Request::patch(
        gh.path(&format!("dependabot/alerts/{number}")),
        serde_json::json!({ "state": "open" }),
    )
}

/// Dependabot's branch directory for a GitHub ecosystem name
/// (`dependabot/npm_and_yarn/…`).
pub fn dependabot_dir(ecosystem: &str) -> &str {
    match ecosystem {
        "npm" => "npm_and_yarn",
        "pip" => "pip",
        "rubygems" => "bundler",
        "go" => "go_modules",
        "rust" => "cargo",
        "actions" => "github_actions",
        "composer" => "composer",
        "maven" => "maven",
        "nuget" => "nuget",
        "pub" => "pub",
        "swift" => "swift",
        "erlang" => "hex",
        other => other,
    }
}

/// The open Dependabot pull request that updates this alert's package, if
/// one exists among `prs` (head `dependabot/<ecosystem>/…` naming the
/// package in its branch, title or body).
pub fn dependabot_pr<'a>(a: &DependabotAlert, prs: &'a [PullRequest]) -> Option<&'a PullRequest> {
    let prefix = format!("dependabot/{}/", dependabot_dir(&a.ecosystem));
    let name = a.package.to_ascii_lowercase();
    let slug = name.trim_start_matches('@').to_owned();
    let mentions = |text: &str| {
        let t = text.to_ascii_lowercase();
        t.split(|c: char| c.is_whitespace() || matches!(c, '`' | '(' | ')' | '[' | ']' | ','))
            .any(|w| w == name || w.trim_matches(|c| c == '"' || c == '\'') == name)
    };
    prs.iter().find(|p| {
        p.head.starts_with(&prefix)
            && (p.head.to_ascii_lowercase().contains(&format!("/{slug}-"))
                || mentions(&p.title)
                || mentions(&p.body))
    })
}

/// osv.dev ecosystem (as in [`Package::ecosystem`]) of a GitHub ecosystem.
pub fn osv_ecosystem(github: &str) -> Option<&'static str> {
    Some(match github {
        "npm" => "npm",
        "pip" => "PyPI",
        "rust" => "crates.io",
        "go" => "Go",
        "rubygems" => "RubyGems",
        "composer" => "Packagist",
        _ => return None,
    })
}

fn dir_of(path: &str) -> &str {
    path.rfind('/').map(|i| &path[..i]).unwrap_or("")
}

/// The locally locked version of the alert's package: from the alert's
/// manifest itself, else a lockfile in the same directory, else anywhere.
pub fn installed_version(a: &DependabotAlert, packages: &[Package]) -> Option<String> {
    let eco = osv_ecosystem(&a.ecosystem)?;
    let same = |p: &&Package| {
        p.ecosystem == eco
            && (p.name == a.package
                || (eco == "PyPI" && p.name.eq_ignore_ascii_case(&a.package.replace('_', "-"))))
    };
    let dir = dir_of(&a.manifest);
    packages
        .iter()
        .filter(same)
        .find(|p| p.file == a.manifest)
        .or_else(|| {
            packages
                .iter()
                .filter(same)
                .find(|p| dir_of(&p.file) == dir)
        })
        .or_else(|| packages.iter().find(same))
        .map(|p| p.version.clone())
}

/// What to do about an alert.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    /// One sentence, e.g. "Update lodash from 4.17.15 to 4.17.21 in web/package-lock.json".
    pub text: String,
    /// A command that performs the update, run in `dir`.
    pub command: Option<String>,
    /// The manifest's directory (`` for the root).
    pub dir: String,
}

/// Fix guidance for an alert, using the locally installed version when
/// known.
pub fn fix_guidance(a: &DependabotAlert, installed: Option<&str>) -> Fix {
    let dir = dir_of(&a.manifest).to_owned();
    let place = if a.manifest.is_empty() {
        String::new()
    } else {
        format!(" in {}", a.manifest)
    };
    let Some(to) = &a.first_patched else {
        return Fix {
            text: format!(
                "No patched version of {} exists yet (vulnerable: {}). Remove or replace the dependency, or dismiss the alert as a tolerable risk until a fix ships.",
                a.package,
                if a.vulnerable_range.is_empty() {
                    "all versions"
                } else {
                    &a.vulnerable_range
                }
            ),
            command: None,
            dir,
        };
    };
    let from = match installed {
        Some(v) => format!("from {v} "),
        None if !a.vulnerable_range.is_empty() => {
            format!("(installed version in {}) ", a.vulnerable_range)
        }
        None => String::new(),
    };
    let text = format!("Update {} {from}to {to} or later{place}.", a.package);
    let pkg = &a.package;
    let base = a.manifest.rsplit('/').next().unwrap_or("");
    let command = match a.ecosystem.as_str() {
        "npm" => Some(if base == "yarn.lock" {
            format!("yarn upgrade {pkg}@^{to}")
        } else if base == "pnpm-lock.yaml" {
            format!("pnpm update {pkg}@^{to}")
        } else if a.relationship == "transitive" {
            format!(
                "npm update {pkg}   # transitive: or add an \"overrides\" entry {{\"{pkg}\": \"^{to}\"}}"
            )
        } else {
            format!("npm install {pkg}@^{to}")
        }),
        "pip" => Some(if base == "Pipfile.lock" || base == "Pipfile" {
            format!("pipenv install \"{pkg}>={to}\"")
        } else if base == "poetry.lock" || base == "pyproject.toml" {
            format!("poetry add \"{pkg}>={to}\"")
        } else {
            format!(
                "pip install \"{pkg}>={to}\"   # then pin {pkg}=={to} in {}",
                if base.is_empty() {
                    "requirements.txt"
                } else {
                    base
                }
            )
        }),
        "rust" => Some(format!("cargo update -p {pkg} --precise {to}")),
        "go" => Some(format!(
            "go get {pkg}@v{} && go mod tidy",
            to.trim_start_matches('v')
        )),
        "rubygems" => Some(format!("bundle update {pkg} --conservative")),
        "composer" => Some(format!("composer update {pkg} --with-dependencies")),
        "nuget" => Some(format!("dotnet add package {pkg} --version {to}")),
        "actions" => Some(format!(
            "# in the workflow: uses: {pkg}@v{}",
            to.trim_start_matches('v')
        )),
        "maven" => Some(format!("# pom.xml: <version>{to}</version> for {pkg}")),
        _ => None,
    };
    Fix { text, command, dir }
}

// ---------------------------------------------------------------------------
// Code scanning
// ---------------------------------------------------------------------------

/// Why a code-scanning alert is dismissed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CodeReason {
    #[default]
    FalsePositive,
    WontFix,
    UsedInTests,
}

impl CodeReason {
    pub const ALL: [CodeReason; 3] = [
        CodeReason::FalsePositive,
        CodeReason::WontFix,
        CodeReason::UsedInTests,
    ];

    pub fn api(self) -> &'static str {
        match self {
            CodeReason::FalsePositive => "false positive",
            CodeReason::WontFix => "won't fix",
            CodeReason::UsedInTests => "used in tests",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            CodeReason::FalsePositive => "False positive",
            CodeReason::WontFix => "Won't fix",
            CodeReason::UsedInTests => "Used in tests",
        }
    }
}

/// One code-scanning alert.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodeAlert {
    pub number: u64,
    /// `open`, `dismissed`, `fixed`.
    pub state: String,
    pub rule_id: String,
    pub rule_name: String,
    pub description: String,
    pub full_description: String,
    pub help: String,
    pub tags: Vec<String>,
    /// The rule's `error` / `warning` / `note`.
    pub rule_severity: String,
    /// `critical` / `high` / `medium` / `low` for security rules.
    pub security_severity: Option<String>,
    pub tool: String,
    pub tool_version: String,
    pub path: String,
    pub start_line: u64,
    pub end_line: u64,
    pub start_column: u64,
    pub end_column: u64,
    pub message: String,
    /// `refs/heads/main`.
    pub git_ref: String,
    pub commit_sha: String,
    pub created: Option<i64>,
    pub updated: Option<i64>,
    pub dismissed_at: Option<i64>,
    pub dismissed_by: Option<String>,
    pub dismissed_reason: Option<String>,
    pub dismissed_comment: Option<String>,
    pub fixed_at: Option<i64>,
    pub html_url: String,
}

impl CodeAlert {
    pub fn is_open(&self) -> bool {
        self.state == "open"
    }

    /// The security severity when the rule has one, else the rule severity.
    pub fn severity(&self) -> Severity {
        match &self.security_severity {
            Some(s) => Severity::parse(s),
            None => Severity::parse(&self.rule_severity),
        }
    }

    /// `src/a.rs:12`.
    pub fn location(&self) -> String {
        if self.start_line > 0 {
            format!("{}:{}", self.path, self.start_line)
        } else {
            self.path.clone()
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawRule {
    id: String,
    name: String,
    severity: String,
    security_severity_level: Option<String>,
    description: String,
    full_description: String,
    help: Option<String>,
    tags: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawTool {
    name: String,
    version: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawLocation {
    path: String,
    start_line: u64,
    end_line: u64,
    start_column: u64,
    end_column: u64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawMessage {
    text: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawInstance {
    #[serde(rename = "ref")]
    git_ref: String,
    commit_sha: String,
    message: Option<RawMessage>,
    location: Option<RawLocation>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawCodeAlert {
    number: u64,
    state: String,
    rule: Option<RawRule>,
    tool: Option<RawTool>,
    most_recent_instance: Option<RawInstance>,
    html_url: String,
    created_at: Option<String>,
    updated_at: Option<String>,
    dismissed_at: Option<String>,
    dismissed_by: Option<RawUser>,
    dismissed_reason: Option<String>,
    dismissed_comment: Option<String>,
    fixed_at: Option<String>,
}

fn code_from_raw(a: RawCodeAlert) -> CodeAlert {
    let rule = a.rule.unwrap_or_default();
    let tool = a.tool.unwrap_or_default();
    let inst = a.most_recent_instance.unwrap_or_default();
    let loc = inst.location.unwrap_or_default();
    CodeAlert {
        number: a.number,
        state: a.state,
        rule_id: rule.id,
        rule_name: rule.name,
        description: rule.description,
        full_description: rule.full_description,
        help: rule.help.unwrap_or_default(),
        tags: rule.tags,
        rule_severity: rule.severity,
        security_severity: rule.security_severity_level.filter(|s| !s.is_empty()),
        tool: tool.name,
        tool_version: tool.version.unwrap_or_default(),
        path: loc.path,
        start_line: loc.start_line,
        end_line: loc.end_line,
        start_column: loc.start_column,
        end_column: loc.end_column,
        message: inst.message.map(|m| m.text).unwrap_or_default(),
        git_ref: inst.git_ref,
        commit_sha: inst.commit_sha,
        created: time(&a.created_at),
        updated: time(&a.updated_at),
        dismissed_at: time(&a.dismissed_at),
        dismissed_by: a.dismissed_by.map(|u| u.login).filter(|s| !s.is_empty()),
        dismissed_reason: a.dismissed_reason,
        dismissed_comment: a.dismissed_comment.filter(|s| !s.is_empty()),
        fixed_at: time(&a.fixed_at),
        html_url: a.html_url,
    }
}

/// Decode a page of `GET code-scanning/alerts`.
pub fn parse_code(body: &[u8]) -> Result<Vec<CodeAlert>> {
    let raw: Vec<RawCodeAlert> = crate::devops::gh::decode(body, "code scanning alerts")?;
    let mut v: Vec<CodeAlert> = raw.into_iter().map(code_from_raw).collect();
    v.sort_by(|a, b| {
        b.severity()
            .cmp(&a.severity())
            .then(b.created.cmp(&a.created))
            .then(b.number.cmp(&a.number))
    });
    Ok(v)
}

pub fn code_endpoint(gh: &Gh, filter: StateFilter) -> String {
    match filter.code_query() {
        Some(s) => gh.path(&format!("code-scanning/alerts?state={s}&per_page=100")),
        None => gh.path("code-scanning/alerts?per_page=100"),
    }
}

/// The first 100 code-scanning alerts in a state (one request).
pub fn fetch_code(
    gh: &Gh,
    filter: StateFilter,
    calls: &mut Vec<ApiCall>,
) -> Result<Vec<CodeAlert>> {
    parse_code(&gh.get(&code_endpoint(gh, filter), calls)?)
}

/// `PATCH code-scanning/alerts/{n}` dismissing an alert.
pub fn code_dismiss(gh: &Gh, number: u64, reason: CodeReason, comment: &str) -> Request {
    let mut body = serde_json::json!({
        "state": "dismissed",
        "dismissed_reason": reason.api(),
    });
    if let Some(c) = comment_value(comment) {
        body["dismissed_comment"] = serde_json::Value::String(c);
    }
    Request::patch(gh.path(&format!("code-scanning/alerts/{number}")), body)
}

/// `PATCH code-scanning/alerts/{n}` reopening an alert.
pub fn code_reopen(gh: &Gh, number: u64) -> Request {
    Request::patch(
        gh.path(&format!("code-scanning/alerts/{number}")),
        serde_json::json!({ "state": "open" }),
    )
}

/// Lines around a location in a working-tree file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snippet {
    /// 1-based number of `lines[0]`.
    pub first: u64,
    pub lines: Vec<String>,
    /// 1-based inclusive range of the flagged lines.
    pub highlight: (u64, u64),
    /// The file changed since the analysis (fewer lines than the alert's).
    pub short: bool,
}

/// Read `context` lines around `start..=end` of `path` in the working tree.
/// `None` when the file is missing, binary or larger than 5 MB.
pub fn snippet(workdir: &Path, path: &str, start: u64, end: u64, context: u64) -> Option<Snippet> {
    if path.is_empty() || start == 0 || path.contains("..") {
        return None;
    }
    let full = workdir.join(path);
    let meta = std::fs::metadata(&full).ok()?;
    if meta.len() > 5 * 1024 * 1024 {
        return None;
    }
    let bytes = std::fs::read(&full).ok()?;
    if bytes.iter().take(8000).any(|b| *b == 0) {
        return None;
    }
    let text = String::from_utf8_lossy(&bytes);
    let all: Vec<&str> = text.lines().collect();
    let end = end.max(start);
    let first = start.saturating_sub(context).max(1);
    let last = (end + context).min(all.len() as u64);
    if first > all.len() as u64 {
        return Some(Snippet {
            first,
            lines: Vec::new(),
            highlight: (start, end),
            short: true,
        });
    }
    let lines = all[(first - 1) as usize..last as usize]
        .iter()
        .map(|l| {
            let l = l.trim_end_matches('\r');
            super::clip(l, 300)
        })
        .collect();
    Some(Snippet {
        first,
        lines,
        highlight: (start, end),
        short: (all.len() as u64) < end,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devops::gh::Method;

    const DEPENDABOT: &str = r#"[{
      "number": 7, "state": "open",
      "dependency": {"package": {"ecosystem": "npm", "name": "lodash"},
        "manifest_path": "web/package-lock.json", "scope": "runtime", "relationship": "transitive"},
      "security_advisory": {"ghsa_id": "GHSA-35jh-r3h4-6jhm", "cve_id": "CVE-2021-23337",
        "summary": "Command Injection in lodash", "description": "lodash versions prior to 4.17.21 are vulnerable",
        "severity": "high", "cvss": {"vector_string": "CVSS:3.1/AV:N/AC:L/PR:H/UI:N/S:U/C:H/I:H/A:H", "score": 7.2},
        "cwes": [{"cwe_id": "CWE-77", "name": "Command Injection"}],
        "epss": {"percentage": 0.0123, "percentile": 0.85},
        "references": [{"url": "https://nvd.nist.gov/vuln/detail/CVE-2021-23337"}, {"url": "https://github.com/advisories/GHSA-35jh-r3h4-6jhm"}],
        "published_at": "2021-02-15T00:00:00Z"},
      "security_vulnerability": {"package": {"ecosystem": "npm", "name": "lodash"}, "severity": "high",
        "vulnerable_version_range": "< 4.17.21", "first_patched_version": {"identifier": "4.17.21"}},
      "html_url": "https://github.com/o/r/security/dependabot/7",
      "created_at": "2026-10-01T10:00:00Z", "updated_at": "2026-10-02T10:00:00Z",
      "dismissed_at": null, "dismissed_by": null, "dismissed_reason": null, "dismissed_comment": null, "fixed_at": null
    }, {
      "number": 8, "state": "dismissed",
      "dependency": {"package": {"ecosystem": "pip", "name": "django"}, "manifest_path": "requirements.txt"},
      "security_advisory": {"ghsa_id": "GHSA-aaaa", "cve_id": null, "summary": "x", "severity": "critical"},
      "security_vulnerability": {"severity": "critical", "vulnerable_version_range": ">= 2.0, < 2.0.2", "first_patched_version": null},
      "html_url": "u", "created_at": "2026-09-01T10:00:00Z",
      "dismissed_at": "2026-09-02T10:00:00Z", "dismissed_by": {"login": "alice"},
      "dismissed_reason": "tolerable_risk", "dismissed_comment": "internal only"
    }]"#;

    #[test]
    fn dependabot_parse_and_sort() {
        let v = parse_dependabot(DEPENDABOT.as_bytes()).unwrap();
        assert_eq!(v.len(), 2);
        // Critical first.
        assert_eq!(v[0].number, 8);
        assert_eq!(v[0].severity, Severity::Critical);
        assert_eq!(v[0].dismissed_by.as_deref(), Some("alice"));
        assert_eq!(v[0].first_patched, None);
        let a = &v[1];
        assert_eq!(a.package, "lodash");
        assert_eq!(a.manifest, "web/package-lock.json");
        assert_eq!(a.vulnerable_range, "< 4.17.21");
        assert_eq!(a.first_patched.as_deref(), Some("4.17.21"));
        assert_eq!(a.ids(), "CVE-2021-23337 · GHSA-35jh-r3h4-6jhm");
        assert_eq!(a.references.len(), 2);
        assert_eq!(a.cvss_score, Some(7.2));
        assert_eq!(a.cwes[0].0, "CWE-77");
        assert_eq!(a.epss.map(|e| e.1), Some(0.85));
        assert_eq!(a.relationship, "transitive");
        assert!(a.is_open());
        let c = severity_counts(v.iter().map(|a| a.severity));
        assert_eq!(c, [1, 1, 0, 0, 0]);
    }

    #[test]
    fn dismiss_and_reopen_bodies() {
        let gh = Gh::new(".", "o", "r");
        let r = dependabot_dismiss(&gh, 7, DependabotReason::NotUsed, "  only in a dev script ");
        assert_eq!(r.method, Method::Patch);
        assert_eq!(r.endpoint, "repos/o/r/dependabot/alerts/7");
        assert_eq!(
            r.body.unwrap(),
            serde_json::json!({"state": "dismissed", "dismissed_reason": "not_used", "dismissed_comment": "only in a dev script"})
        );
        let r = dependabot_dismiss(&gh, 7, DependabotReason::FixStarted, "");
        assert_eq!(
            r.body.unwrap(),
            serde_json::json!({"state": "dismissed", "dismissed_reason": "fix_started"})
        );
        let long = "x".repeat(400);
        let r = dependabot_dismiss(&gh, 1, DependabotReason::TolerableRisk, &long);
        assert_eq!(
            r.body.unwrap()["dismissed_comment"].as_str().unwrap().len(),
            MAX_COMMENT
        );
        let r = dependabot_reopen(&gh, 7);
        assert_eq!(r.method, Method::Patch);
        assert_eq!(r.body.unwrap(), serde_json::json!({"state": "open"}));
        let r = code_dismiss(&gh, 3, CodeReason::WontFix, "legacy");
        assert_eq!(r.endpoint, "repos/o/r/code-scanning/alerts/3");
        assert_eq!(
            r.body.unwrap(),
            serde_json::json!({"state": "dismissed", "dismissed_reason": "won't fix", "dismissed_comment": "legacy"})
        );
        let r = code_dismiss(&gh, 3, CodeReason::UsedInTests, "");
        assert_eq!(r.body.unwrap()["dismissed_reason"], "used in tests");
        let r = code_dismiss(&gh, 3, CodeReason::FalsePositive, "");
        assert_eq!(r.body.unwrap()["dismissed_reason"], "false positive");
        let r = code_reopen(&gh, 3);
        assert_eq!(r.endpoint, "repos/o/r/code-scanning/alerts/3");
        assert_eq!(r.body.unwrap(), serde_json::json!({"state": "open"}));
        assert_eq!(
            dependabot_endpoint(&gh, StateFilter::Open),
            "repos/o/r/dependabot/alerts?state=open&per_page=100"
        );
        assert_eq!(
            dependabot_endpoint(&gh, StateFilter::All),
            "repos/o/r/dependabot/alerts?per_page=100"
        );
        assert_eq!(
            code_endpoint(&gh, StateFilter::Fixed),
            "repos/o/r/code-scanning/alerts?state=fixed&per_page=100"
        );
    }

    fn pr(head: &str, title: &str) -> PullRequest {
        PullRequest {
            number: 12,
            title: title.into(),
            author: "dependabot[bot]".into(),
            head: head.into(),
            head_sha: String::new(),
            head_repo: None,
            base: "main".into(),
            draft: false,
            url: String::new(),
            created: None,
            updated: None,
            labels: Vec::new(),
            assignees: Vec::new(),
            reviewers: Vec::new(),
            milestone: None,
            body: String::new(),
        }
    }

    #[test]
    fn guidance_and_prs() {
        let v = parse_dependabot(DEPENDABOT.as_bytes()).unwrap();
        let lodash = v.iter().find(|a| a.package == "lodash").unwrap();
        let pkgs = vec![
            Package {
                ecosystem: "npm",
                name: "lodash".into(),
                version: "4.17.10".into(),
                file: "docs/package-lock.json".into(),
            },
            Package {
                ecosystem: "npm",
                name: "lodash".into(),
                version: "4.17.15".into(),
                file: "web/package-lock.json".into(),
            },
        ];
        let installed = installed_version(lodash, &pkgs);
        assert_eq!(installed.as_deref(), Some("4.17.15"));
        let f = fix_guidance(lodash, installed.as_deref());
        assert_eq!(
            f.text,
            "Update lodash from 4.17.15 to 4.17.21 or later in web/package-lock.json."
        );
        assert_eq!(f.dir, "web");
        assert!(f.command.unwrap().starts_with("npm update lodash"));
        let django = v.iter().find(|a| a.package == "django").unwrap();
        let f = fix_guidance(django, None);
        assert!(f.text.starts_with("No patched version of django"));
        assert!(f.command.is_none());

        let prs = vec![
            pr("feature/x", "Bump lodash"),
            pr(
                "dependabot/npm_and_yarn/web/lodash-4.17.21",
                "Bump lodash from 4.17.15 to 4.17.21 in /web",
            ),
        ];
        assert_eq!(
            dependabot_pr(lodash, &prs).map(|p| p.head.as_str()),
            Some("dependabot/npm_and_yarn/web/lodash-4.17.21")
        );
        let mut grouped = pr(
            "dependabot/npm_and_yarn/npm_and_yarn-1a2b",
            "Bump the npm group with 3 updates",
        );
        grouped.body = "Updates `lodash` from 4.17.15 to 4.17.21".into();
        assert!(dependabot_pr(lodash, &[grouped]).is_some());
        assert!(dependabot_pr(django, &prs).is_none());
        assert!(dependabot_pr(lodash, &[pr("dependabot/pip/lodash-1", "Bump lodash")]).is_none());
    }

    const CODE: &str = r##"[{
      "number": 4, "state": "open", "created_at": "2026-10-01T10:00:00Z", "updated_at": null,
      "html_url": "https://github.com/o/r/security/code-scanning/4",
      "rule": {"id": "js/zipslip", "name": "js/zipslip", "severity": "error", "security_severity_level": "high",
        "description": "Arbitrary file write during zip extraction", "full_description": "Extracting files from a malicious zip archive",
        "tags": ["security", "external/cwe/cwe-022"], "help": "# Arbitrary file write"},
      "tool": {"name": "CodeQL", "guid": null, "version": "2.15.0"},
      "most_recent_instance": {"ref": "refs/heads/main", "commit_sha": "abc", "state": "open",
        "message": {"text": "Unsanitized archive entry"},
        "location": {"path": "src/unzip.js", "start_line": 3, "end_line": 4, "start_column": 7, "end_column": 18}}
    }, {
      "number": 5, "state": "dismissed",
      "rule": {"id": "py/unused", "severity": "note", "description": "Unused import"},
      "tool": {"name": "CodeQL"},
      "most_recent_instance": {"location": {"path": "a.py", "start_line": 1}},
      "dismissed_reason": "false positive", "dismissed_by": {"login": "bob"}
    }]"##;

    #[test]
    fn code_parse_and_snippet() {
        let v = parse_code(CODE.as_bytes()).unwrap();
        assert_eq!(v[0].number, 4);
        assert_eq!(v[0].severity(), Severity::High);
        assert_eq!(v[0].location(), "src/unzip.js:3");
        assert_eq!(v[0].tool_version, "2.15.0");
        assert_eq!(v[1].severity(), Severity::Low);
        assert_eq!(v[1].dismissed_reason.as_deref(), Some("false positive"));
        let dir = std::env::temp_dir().join(format!("hadion-govern-snip-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src/unzip.js"),
            "const a = 1;\nconst b = 2;\nentry.pipe(fs.createWriteStream(entry.path));\nmore();\nend();\n",
        )
        .unwrap();
        let s = snippet(&dir, "src/unzip.js", 3, 4, 1).unwrap();
        assert_eq!(s.first, 2);
        assert_eq!(s.lines.len(), 4);
        assert_eq!(s.highlight, (3, 4));
        assert!(!s.short);
        assert!(snippet(&dir, "missing.js", 1, 1, 2).is_none());
        assert!(snippet(&dir, "../x", 1, 1, 2).is_none());
        let s = snippet(&dir, "src/unzip.js", 40, 40, 2).unwrap();
        assert!(s.short);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn severities() {
        assert_eq!(Severity::parse("moderate"), Severity::Medium);
        assert_eq!(Severity::parse("ERROR"), Severity::High);
        assert_eq!(Severity::from_score(9.8), Severity::Critical);
        assert_eq!(Severity::from_score(5.0), Severity::Medium);
        assert!(Severity::Critical > Severity::High);
    }
}
