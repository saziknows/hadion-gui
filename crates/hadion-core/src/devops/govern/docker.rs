//! Container images: Dockerfile discovery and parsing (stages, USER,
//! EXPOSE, ADD / COPY, RUN), lint rules with explanations and fixes, image
//! references in compose files, and the image scanners (`trivy`, `grype`,
//! `docker scout`) with parsers for their JSON / SARIF reports.
//!
//! Only names of ENV / ARG variables are ever recorded, never their values.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use super::alerts::Severity;
use crate::Result;
use crate::devops::proc::ProcSpec;

fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// `Dockerfile`, `Dockerfile.*`, `*.dockerfile`, `Containerfile`, `Containerfile.*`.
pub fn is_dockerfile(path: &str) -> bool {
    let b = basename(path);
    let l = b.to_ascii_lowercase();
    l == "dockerfile"
        || l.starts_with("dockerfile.")
        || l.ends_with(".dockerfile")
        || l == "containerfile"
        || l.starts_with("containerfile.")
}

/// `docker-compose*.yml`, `compose*.yaml`, …
pub fn is_compose(path: &str) -> bool {
    let l = basename(path).to_ascii_lowercase();
    (l.starts_with("docker-compose") || l.starts_with("compose"))
        && (l.ends_with(".yml") || l.ends_with(".yaml"))
}

/// One instruction, continuation lines joined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instruction {
    /// 1-based first line.
    pub line: usize,
    pub end_line: usize,
    /// Upper-case keyword: `FROM`, `RUN`, …
    pub keyword: String,
    /// Everything after the keyword (heredoc bodies appended on new lines).
    pub args: String,
}

/// An image reference split into parts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImageRef {
    pub text: String,
    /// Registry and repository, e.g. `ghcr.io/acme/app` or `node`.
    pub repo: String,
    pub tag: Option<String>,
    pub digest: Option<String>,
}

impl ImageRef {
    pub fn parse(text: &str) -> Self {
        let text = text.trim().to_owned();
        let (rest, digest) = match text.split_once('@') {
            Some((r, d)) => (r.to_owned(), Some(d.to_owned())),
            None => (text.clone(), None),
        };
        let slash = rest.rfind('/').map(|i| i + 1).unwrap_or(0);
        let (repo, tag) = match rest[slash..].rfind(':') {
            Some(i) => (
                rest[..slash + i].to_owned(),
                Some(rest[slash + i + 1..].to_owned()),
            ),
            None => (rest.clone(), None),
        };
        Self {
            text,
            repo,
            tag,
            digest,
        }
    }

    /// Uses a build argument (`$BASE`, `${NODE_VERSION}`).
    pub fn uses_arg(&self) -> bool {
        self.text.contains('$')
    }
}

/// One build stage.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stage {
    pub index: usize,
    /// Line of its FROM.
    pub line: usize,
    pub image: ImageRef,
    /// `AS name`.
    pub name: Option<String>,
    pub platform: Option<String>,
    /// FROM names an earlier stage.
    pub from_stage: bool,
    pub scratch: bool,
    /// `(line, user)` of each USER.
    pub users: Vec<(usize, String)>,
    /// `(line, port)` of each exposed port.
    pub exposes: Vec<(usize, String)>,
    /// `(line, ADD|COPY, arguments)`.
    pub copies: Vec<(usize, String, String)>,
    pub runs: usize,
    /// `(line, NONE|CMD …)`.
    pub healthcheck: Option<(usize, String)>,
    /// `(line, ENV|ARG, name)`.
    pub variables: Vec<(usize, String, String)>,
    pub workdir: Option<String>,
    /// Indices into `Dockerfile::instructions`.
    pub instructions: Vec<usize>,
}

impl Stage {
    pub fn label(&self) -> String {
        match &self.name {
            Some(n) => format!("{} (stage {})", n, self.index + 1),
            None => format!("Stage {}", self.index + 1),
        }
    }

    /// The last USER of the stage.
    pub fn final_user(&self) -> Option<&str> {
        self.users.last().map(|(_, u)| u.as_str())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dockerfile {
    pub path: String,
    pub lines: usize,
    pub instructions: Vec<Instruction>,
    pub stages: Vec<Stage>,
    /// ARG names declared before the first FROM.
    pub global_args: Vec<String>,
    pub escape: char,
}

fn heredoc_marker(args: &str) -> Option<(String, bool)> {
    let i = args.find("<<")?;
    let rest = &args[i + 2..];
    let (dash, rest) = match rest.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, rest),
    };
    let rest = rest.trim_start_matches(['"', '\'']);
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        None
    } else {
        Some((name, dash))
    }
}

/// Parse a Dockerfile.
pub fn parse(path: &str, text: &str) -> Dockerfile {
    let lines: Vec<&str> = text.lines().collect();
    let mut escape = '\\';
    // Parser directives are only recognised at the very top.
    for l in &lines {
        let t = l.trim();
        if let Some(d) = t.strip_prefix('#') {
            let d = d.trim().to_ascii_lowercase();
            if let Some(v) = d.strip_prefix("escape=") {
                if v.trim() == "`" {
                    escape = '`';
                }
                continue;
            }
            if d.starts_with("syntax=") || d.starts_with("check=") {
                continue;
            }
        }
        break;
    }
    let mut out = Dockerfile {
        path: path.to_owned(),
        lines: lines.len(),
        escape,
        ..Default::default()
    };
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim();
        if t.is_empty() || t.starts_with('#') {
            i += 1;
            continue;
        }
        let start = i;
        let mut joined = String::new();
        loop {
            let raw = lines[i].trim_end();
            let cont = raw.ends_with(escape);
            let part = if cont {
                &raw[..raw.len() - escape.len_utf8()]
            } else {
                raw
            };
            let p = part.trim();
            if !(i > start && p.starts_with('#')) {
                if !joined.is_empty() {
                    joined.push(' ');
                }
                joined.push_str(p);
            }
            i += 1;
            if !cont || i >= lines.len() {
                break;
            }
            // Blank and comment lines inside a continuation are skipped.
            while i < lines.len() && {
                let n = lines[i].trim();
                n.is_empty() || n.starts_with('#')
            } {
                i += 1;
            }
            if i >= lines.len() {
                break;
            }
        }
        let (kw, args) = match joined.split_once(char::is_whitespace) {
            Some((k, a)) => (k.to_ascii_uppercase(), a.trim().to_owned()),
            None => (joined.to_ascii_uppercase(), String::new()),
        };
        let mut args = args;
        if let Some((marker, _)) = heredoc_marker(&args) {
            while i < lines.len() {
                let l = lines[i];
                i += 1;
                if l.trim() == marker {
                    break;
                }
                args.push('\n');
                args.push_str(l);
            }
        }
        out.instructions.push(Instruction {
            line: start + 1,
            end_line: i,
            keyword: kw,
            args,
        });
    }
    // Stages.
    let mut names: Vec<String> = Vec::new();
    for (k, ins) in out.instructions.iter().enumerate() {
        if ins.keyword == "FROM" {
            let mut platform = None;
            let mut tokens = Vec::new();
            for tok in ins.args.split_whitespace() {
                if let Some(p) = tok.strip_prefix("--platform=") {
                    platform = Some(p.to_owned());
                } else if !tok.starts_with("--") {
                    tokens.push(tok);
                }
            }
            let image_text = tokens.first().copied().unwrap_or("");
            let name = match (tokens.get(1), tokens.get(2)) {
                (Some(a), Some(n)) if a.eq_ignore_ascii_case("as") => Some((*n).to_owned()),
                _ => None,
            };
            let from_stage = names.iter().any(|n| n.eq_ignore_ascii_case(image_text));
            if let Some(n) = &name {
                names.push(n.clone());
            }
            out.stages.push(Stage {
                index: out.stages.len(),
                line: ins.line,
                image: ImageRef::parse(image_text),
                name,
                platform,
                from_stage,
                scratch: image_text.eq_ignore_ascii_case("scratch"),
                instructions: vec![k],
                ..Default::default()
            });
            continue;
        }
        let Some(stage) = out.stages.last_mut() else {
            if ins.keyword == "ARG" {
                out.global_args.extend(arg_names(&ins.args));
            }
            continue;
        };
        stage.instructions.push(k);
        match ins.keyword.as_str() {
            "USER" => stage.users.push((
                ins.line,
                ins.args.split_whitespace().next().unwrap_or("").to_owned(),
            )),
            "EXPOSE" => {
                for p in ins.args.split_whitespace() {
                    stage.exposes.push((ins.line, p.to_owned()));
                }
            }
            "ADD" | "COPY" => stage
                .copies
                .push((ins.line, ins.keyword.clone(), ins.args.clone())),
            "RUN" => stage.runs += 1,
            "HEALTHCHECK" => stage.healthcheck = Some((ins.line, clip_words(&ins.args, 8))),
            "WORKDIR" => stage.workdir = Some(ins.args.clone()),
            "ENV" => {
                for n in env_names(&ins.args) {
                    stage.variables.push((ins.line, "ENV".into(), n));
                }
            }
            "ARG" => {
                for n in arg_names(&ins.args) {
                    stage.variables.push((ins.line, "ARG".into(), n));
                }
            }
            _ => {}
        }
    }
    out
}

fn clip_words(s: &str, n: usize) -> String {
    let w: Vec<&str> = s.split_whitespace().collect();
    if w.len() > n {
        format!("{} …", w[..n].join(" "))
    } else {
        w.join(" ")
    }
}

/// Names declared by `ENV A=1 B=2` or `ENV A 1` (values dropped).
pub fn env_names(args: &str) -> Vec<String> {
    let a = args.trim();
    let first = a.split_whitespace().next().unwrap_or("");
    if !first.contains('=') {
        return if first.is_empty() {
            Vec::new()
        } else {
            vec![first.to_owned()]
        };
    }
    // `A=1 B="two words"`: names are the text before each `=` at the start
    // of a token outside quotes.
    let mut names = Vec::new();
    let mut quote: Option<char> = None;
    let mut token = String::new();
    let mut at_start = true;
    for c in a.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c.is_whitespace() => {
                at_start = true;
                token.clear();
            }
            None if c == '=' && at_start => {
                names.push(std::mem::take(&mut token));
                at_start = false;
            }
            None => {
                if at_start {
                    token.push(c);
                }
            }
        }
    }
    names.retain(|n| !n.is_empty());
    names
}

/// Names declared by `ARG A[=default] [B…]`.
pub fn arg_names(args: &str) -> Vec<String> {
    args.split_whitespace()
        .map(|t| t.split('=').next().unwrap_or("").to_owned())
        .filter(|n| !n.is_empty())
        .collect()
}

/// Whether a variable name looks like it holds a secret.
pub fn secret_name(name: &str) -> bool {
    let u = name.to_ascii_uppercase();
    let parts: Vec<&str> = u.split(['_', '-', '.']).filter(|p| !p.is_empty()).collect();
    if matches!(
        parts.last(),
        Some(&("FILE" | "PATH" | "DIR" | "URL" | "ID" | "NAME" | "USER" | "USERNAME"))
    ) {
        return false;
    }
    const WORDS: [&str; 9] = [
        "PASSWORD",
        "PASSWD",
        "PASSPHRASE",
        "SECRET",
        "SECRETS",
        "TOKEN",
        "APIKEY",
        "CREDENTIALS",
        "CREDENTIAL",
    ];
    if parts
        .iter()
        .any(|p| WORDS.contains(p) || *p == "PWD" || *p == "AUTH")
    {
        return true;
    }
    parts.windows(2).any(|w| {
        w[1] == "KEY"
            && matches!(
                w[0],
                "API" | "PRIVATE" | "ACCESS" | "SECRET" | "SIGNING" | "ENCRYPTION" | "LICENSE"
            )
    })
}

/// How serious a lint finding is.
pub type Level = Severity;

/// A lint finding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub rule: &'static str,
    pub severity: Severity,
    pub line: usize,
    pub title: String,
    pub explanation: &'static str,
    pub fix: String,
}

fn words(s: &str) -> Vec<&str> {
    s.split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')'))
        .filter(|w| !w.is_empty())
        .collect()
}

/// Each shell command of a RUN (split on `&&`, `;`, `||`, newlines).
fn commands(run: &str) -> Vec<String> {
    run.replace("&&", "\n")
        .replace("||", "\n")
        .split(['\n', ';'])
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

fn pipes_to_shell(run: &str) -> bool {
    for cmd in run.split("&&").flat_map(|c| c.split(';')) {
        let segs: Vec<&str> = cmd.split('|').map(str::trim).collect();
        if segs.len() < 2 {
            continue;
        }
        let fetch = segs[..segs.len() - 1].iter().any(|s| {
            let w = words(s);
            w.first().is_some_and(|f| matches!(*f, "curl" | "wget"))
                || w.iter().any(|x| *x == "curl" || *x == "wget")
        });
        let shell = segs[1..].iter().any(|s| {
            let w = words(s);
            let first = w
                .iter()
                .find(|x| **x != "sudo" && !x.starts_with('-'))
                .copied();
            matches!(
                first,
                Some(
                    "sh" | "bash"
                        | "zsh"
                        | "ash"
                        | "dash"
                        | "ksh"
                        | "python"
                        | "python3"
                        | "perl"
                        | "ruby"
                        | "node"
                )
            )
        });
        if fetch && shell {
            return true;
        }
    }
    false
}

fn installs_deps(cmd: &str) -> Option<&'static str> {
    let w = words(cmd);
    let has = |a: &str, b: &str| w.windows(2).any(|p| p[0] == a && p[1] == b);
    if has("npm", "install") || has("npm", "ci") || has("npm", "i") {
        Some("npm")
    } else if has("yarn", "install") || w == ["yarn"] {
        Some("yarn")
    } else if has("pnpm", "install") {
        Some("pnpm")
    } else if has("pip", "install")
        || has("pip3", "install")
        || has("poetry", "install")
        || has("pipenv", "install")
    {
        Some("pip")
    } else if has("composer", "install") {
        Some("composer")
    } else if has("bundle", "install") {
        Some("bundler")
    } else if has("go", "mod") || has("go", "build") {
        Some("go")
    } else if has("cargo", "fetch") || has("cargo", "build") {
        Some("cargo")
    } else if has("dotnet", "restore") {
        Some("dotnet")
    } else if w.iter().any(|x| *x == "mvn" || *x == "./mvnw") && cmd.contains("dependency:") {
        Some("maven")
    } else {
        None
    }
}

/// `COPY . .`, `COPY ./ /app`, `ADD . .` (copies the whole context).
fn copies_everything(args: &str) -> bool {
    let toks: Vec<&str> = args
        .split_whitespace()
        .filter(|t| !t.starts_with("--"))
        .collect();
    toks.len() >= 2
        && toks[..toks.len() - 1]
            .iter()
            .any(|s| *s == "." || *s == "./")
}

/// Lint a parsed Dockerfile.
pub fn lint(df: &Dockerfile) -> Vec<Finding> {
    let mut out = Vec::new();
    let last = df.stages.len().saturating_sub(1);
    for st in &df.stages {
        // Base images.
        if !st.scratch && !st.from_stage {
            let img = &st.image;
            if img.uses_arg() {
                // Resolved at build time; nothing to say about the tag.
            } else if img.digest.is_none()
                && (img.tag.is_none() || img.tag.as_deref() == Some("latest"))
            {
                out.push(Finding {
                    rule: "unpinned-tag",
                    severity: Severity::Medium,
                    line: st.line,
                    title: format!(
                        "Base image {} {}",
                        img.text,
                        if img.tag.is_none() { "has no tag" } else { "uses latest" }
                    ),
                    explanation: "Without a tag (or with latest) every build may pull a different image: builds stop being reproducible and a breaking or compromised release lands silently.",
                    fix: format!("FROM {}:<version>   # e.g. a specific release such as 1.27-alpine", img.repo),
                });
            }
            if img.digest.is_none() && !img.uses_arg() {
                out.push(Finding {
                    rule: "no-digest",
                    severity: Severity::Unknown,
                    line: st.line,
                    title: format!("Base image {} is not pinned by digest", img.text),
                    explanation: "A tag can be moved to another image. A digest names exactly one image; Dependabot and Renovate keep digests up to date.",
                    fix: format!(
                        "docker buildx imagetools inspect {}   # then: FROM {}@sha256:<digest>",
                        img.text, img.text
                    ),
                });
            }
        }
        for ins in st.instructions.iter().map(|k| &df.instructions[*k]) {
            match ins.keyword.as_str() {
                "ADD" => {
                    let has_checksum = ins.args.contains("--checksum=");
                    if !has_checksum
                        && ins
                            .args
                            .split_whitespace()
                            .any(|t| t.starts_with("http://") || t.starts_with("https://"))
                    {
                        out.push(Finding {
                            rule: "add-url",
                            severity: Severity::Medium,
                            line: ins.line,
                            title: "ADD downloads from a URL without a checksum".into(),
                            explanation: "ADD fetches the URL at build time and trusts whatever the server sends; the download is not verified and cannot be cached reliably.",
                            fix: "ADD --checksum=sha256:<hash> https://… /dest   # BuildKit, or:\nRUN curl -fsSLo /tmp/f https://… && echo \"<sha256>  /tmp/f\" | sha256sum -c -".into(),
                        });
                    }
                }
                "RUN" => {
                    let run = &ins.args;
                    if pipes_to_shell(run) {
                        out.push(Finding {
                            rule: "curl-pipe-sh",
                            severity: Severity::High,
                            line: ins.line,
                            title: "A downloaded script is piped straight into a shell".into(),
                            explanation: "Whatever the server returns runs as root inside the build, unverified. A compromised or hijacked host compromises the image.",
                            fix: "RUN curl -fsSLo install.sh https://… \\\n && echo \"<sha256>  install.sh\" | sha256sum -c - \\\n && sh install.sh && rm install.sh".into(),
                        });
                    }
                    let cmds = commands(run);
                    let apt = cmds.iter().any(|c| {
                        let w = words(c);
                        w.windows(2)
                            .any(|p| (p[0] == "apt-get" || p[0] == "apt") && p[1] == "install")
                            || (w.contains(&"apt-get") && w.contains(&"install"))
                    });
                    if apt {
                        if !run.contains("--no-install-recommends") {
                            out.push(Finding {
                                rule: "apt-recommends",
                                severity: Severity::Low,
                                line: ins.line,
                                title: "apt-get install without --no-install-recommends".into(),
                                explanation: "Recommended packages are installed too: a bigger image and more software to patch, none of it asked for.",
                                fix: "apt-get install -y --no-install-recommends <packages>".into(),
                            });
                        }
                        let cache_mount = run.contains("target=/var/lib/apt")
                            || run.contains("target=/var/cache/apt");
                        if !run.contains("/var/lib/apt/lists") && !cache_mount {
                            out.push(Finding {
                                rule: "apt-lists",
                                severity: Severity::Low,
                                line: ins.line,
                                title: "apt lists are left in the image".into(),
                                explanation: "The package index downloaded by apt-get update stays in this layer (tens of MB) unless it is removed in the same RUN.",
                                fix: "RUN apt-get update && apt-get install -y --no-install-recommends <packages> \\\n && rm -rf /var/lib/apt/lists/*".into(),
                            });
                        }
                    }
                    let apk = cmds.iter().any(|c| {
                        let w = words(c);
                        w.windows(2).any(|p| p[0] == "apk" && p[1] == "add")
                    });
                    if apk && !run.contains("--no-cache") && !run.contains("/var/cache/apk") {
                        out.push(Finding {
                            rule: "apk-cache",
                            severity: Severity::Low,
                            line: ins.line,
                            title: "apk add without --no-cache".into(),
                            explanation: "The apk index is kept in the layer. --no-cache fetches it on the fly and leaves nothing behind.",
                            fix: "apk add --no-cache <packages>".into(),
                        });
                    }
                    if cmds.iter().any(|c| words(c).contains(&"sudo")) {
                        out.push(Finding {
                            rule: "sudo",
                            severity: Severity::Medium,
                            line: ins.line,
                            title: "sudo in a RUN instruction".into(),
                            explanation: "Builds run as root already (or as the current USER on purpose). sudo adds a setuid binary to the image and unpredictable TTY and signal behaviour.",
                            fix: "Drop sudo; switch with USER root … USER app around the steps that need root.".into(),
                        });
                    }
                }
                "EXPOSE" => {
                    for p in ins.args.split_whitespace() {
                        let port = p.split('/').next().unwrap_or(p);
                        if port == "22" {
                            out.push(Finding {
                                rule: "expose-ssh",
                                severity: Severity::Medium,
                                line: ins.line,
                                title: "EXPOSE 22 (SSH)".into(),
                                explanation: "Running an SSH server in a container widens the attack surface and bypasses the runtime's own access (docker exec, kubectl exec).",
                                fix: "Remove the SSH server and EXPOSE 22; use docker exec / kubectl exec for access.".into(),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        // Secret-looking variable names (never their values).
        for (line, kind, name) in &st.variables {
            if secret_name(name) {
                let env = kind == "ENV";
                out.push(Finding {
                    rule: "secret-variable",
                    severity: if env { Severity::High } else { Severity::Medium },
                    line: *line,
                    title: format!("{kind} {name} looks like a secret"),
                    explanation: if env {
                        "ENV values are baked into the image configuration: anyone who can pull the image reads them with docker inspect."
                    } else {
                        "Build arguments are recorded in the image history (docker history) and in build caches."
                    },
                    fix: "RUN --mount=type=secret,id=token TOKEN=$(cat /run/secrets/token) …   # docker build --secret id=token,env=TOKEN".into(),
                });
            }
        }
        // COPY of the whole context before the dependency install.
        let mut copied_all: Option<usize> = None;
        for ins in st.instructions.iter().map(|k| &df.instructions[*k]) {
            if (ins.keyword == "COPY" || ins.keyword == "ADD")
                && copies_everything(&ins.args)
                && copied_all.is_none()
            {
                copied_all = Some(ins.line);
            }
            if ins.keyword == "RUN"
                && let Some(copy_line) = copied_all
                && let Some(tool) = commands(&ins.args).iter().find_map(|c| installs_deps(c))
            {
                out.push(Finding {
                    rule: "copy-before-install",
                    severity: Severity::Medium,
                    line: copy_line,
                    title: format!("COPY of the whole context before the {tool} install (line {})", ins.line),
                    explanation: "Any change to any file invalidates the cache from the COPY on, so every build reinstalls all dependencies.",
                    fix: match tool {
                        "npm" | "yarn" | "pnpm" => "COPY package.json package-lock.json ./\nRUN npm ci\nCOPY . .".to_owned(),
                        "pip" => "COPY requirements.txt ./\nRUN pip install -r requirements.txt\nCOPY . .".to_owned(),
                        "composer" => "COPY composer.json composer.lock ./\nRUN composer install --no-scripts\nCOPY . .".to_owned(),
                        "bundler" => "COPY Gemfile Gemfile.lock ./\nRUN bundle install\nCOPY . .".to_owned(),
                        "go" => "COPY go.mod go.sum ./\nRUN go mod download\nCOPY . .".to_owned(),
                        "cargo" => "COPY Cargo.toml Cargo.lock ./\nRUN cargo fetch\nCOPY . .".to_owned(),
                        "dotnet" => "COPY *.csproj ./\nRUN dotnet restore\nCOPY . .".to_owned(),
                        _ => "Copy the dependency manifests first, install, then COPY . .".to_owned(),
                    },
                });
                break;
            }
        }
        if st.index == last {
            // The final stage is what runs.
            match st.final_user() {
                None if !st.scratch => out.push(Finding {
                    rule: "root-user",
                    severity: Severity::Medium,
                    line: st.line,
                    title: "No USER: the container runs as root".into(),
                    explanation: "Unless the base image sets a user, the process runs as root; a compromise of the app is then root in the container, one step from the host.",
                    fix: "RUN adduser --system --uid 10001 app   # (addgroup/adduser -S on Alpine)\nUSER 10001".into(),
                }),
                Some(u) if is_root(u) => {
                    let line = st.users.last().map(|(l, _)| *l).unwrap_or(st.line);
                    out.push(Finding {
                        rule: "root-user",
                        severity: Severity::Medium,
                        line,
                        title: format!("USER {u}: the container runs as root"),
                        explanation: "The last USER of the final stage decides who the process runs as. Switch to an unprivileged user after the steps that need root.",
                        fix: "USER 10001   # or a named user created earlier".into(),
                    });
                }
                _ => {}
            }
            if st.healthcheck.is_none() && !st.scratch {
                out.push(Finding {
                    rule: "no-healthcheck",
                    severity: Severity::Unknown,
                    line: st.line,
                    title: "No HEALTHCHECK".into(),
                    explanation: "Docker and Compose cannot tell a hung process from a healthy one. Kubernetes ignores HEALTHCHECK (it uses probes), so this matters for plain Docker and Compose.",
                    fix: "HEALTHCHECK --interval=30s --timeout=3s CMD wget -qO- http://localhost:8080/health || exit 1".into(),
                });
            }
        }
    }
    out.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.line.cmp(&b.line)));
    out
}

fn is_root(user: &str) -> bool {
    let u = user.split(':').next().unwrap_or(user);
    u == "root" || u == "0"
}

/// An image a compose file uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposeImage {
    pub file: String,
    pub service: String,
    /// `None` when the service only builds.
    pub image: Option<ImageRef>,
    /// The build context or Dockerfile, when the service builds.
    pub build: Option<String>,
    pub line: usize,
}

/// Services and their images in a compose file.
pub fn compose_images(path: &str, text: &str) -> Vec<ComposeImage> {
    let Ok(mut v) = serde_yaml::from_str::<serde_yaml::Value>(text) else {
        return Vec::new();
    };
    // `<<: *defaults` merge keys (shared service settings).
    let _ = v.apply_merge();
    let Some(services) = v.get("services").and_then(|s| s.as_mapping()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (name, svc) in services {
        let service = name.as_str().unwrap_or_default().to_owned();
        let image = svc
            .get("image")
            .and_then(|i| i.as_str())
            .map(ImageRef::parse);
        let build = svc.get("build").map(|b| match b {
            serde_yaml::Value::String(s) => s.clone(),
            other => {
                let ctx = other.get("context").and_then(|c| c.as_str()).unwrap_or(".");
                match other.get("dockerfile").and_then(|d| d.as_str()) {
                    Some(d) => format!("{ctx} ({d})"),
                    None => ctx.to_owned(),
                }
            }
        });
        let needle = image.as_ref().map(|i| i.text.clone()).unwrap_or_default();
        let line = text
            .lines()
            .position(|l| {
                let t = l.trim();
                (!needle.is_empty() && t.starts_with("image:") && t.contains(&needle))
                    || (needle.is_empty() && t.starts_with(&format!("{service}:")))
            })
            .map(|i| i + 1)
            .unwrap_or(0);
        out.push(ComposeImage {
            file: path.to_owned(),
            service,
            image,
            build,
            line,
        });
    }
    out
}

/// Everything found in the working tree.
#[derive(Clone, Debug, Default)]
pub struct ImagesReport {
    pub dockerfiles: Vec<(Dockerfile, Vec<Finding>)>,
    pub compose: Vec<ComposeImage>,
    /// Files that could not be read: `(path, error)`.
    pub unreadable: Vec<(String, String)>,
    pub elapsed_ms: u64,
}

impl ImagesReport {
    /// Distinct external images referenced by FROM and compose `image:`.
    pub fn images(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for (df, _) in &self.dockerfiles {
            for s in &df.stages {
                if !s.scratch && !s.from_stage && !s.image.uses_arg() && !s.image.text.is_empty() {
                    v.push(s.image.text.clone());
                }
            }
        }
        for c in &self.compose {
            if let Some(i) = &c.image
                && !i.uses_arg()
            {
                v.push(i.text.clone());
            }
        }
        v.sort();
        v.dedup();
        v
    }

    pub fn finding_count(&self, at_least: Severity) -> usize {
        self.dockerfiles
            .iter()
            .flat_map(|(_, f)| f)
            .filter(|f| f.severity >= at_least)
            .count()
    }
}

/// Find and lint every Dockerfile and compose file among `tracked`.
pub fn scan_tree(workdir: &Path, tracked: &[String]) -> ImagesReport {
    let started = std::time::Instant::now();
    let mut out = ImagesReport::default();
    for p in tracked {
        let docker = is_dockerfile(p);
        let compose = is_compose(p);
        if !docker && !compose {
            continue;
        }
        let full = workdir.join(p);
        if std::fs::metadata(&full).is_ok_and(|m| m.len() > 2 * 1024 * 1024) {
            out.unreadable.push((p.clone(), "larger than 2 MB".into()));
            continue;
        }
        match std::fs::read_to_string(&full) {
            Ok(text) => {
                if docker {
                    let df = parse(p, &text);
                    let f = lint(&df);
                    out.dockerfiles.push((df, f));
                } else {
                    out.compose.extend(compose_images(p, &text));
                }
            }
            Err(e) => out.unreadable.push((p.clone(), e.to_string())),
        }
    }
    out.dockerfiles.sort_by(|a, b| a.0.path.cmp(&b.0.path));
    out.elapsed_ms = started.elapsed().as_millis() as u64;
    out
}

// ---------------------------------------------------------------------------
// Image scanners
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scanner {
    Trivy,
    Grype,
    Scout,
}

impl Scanner {
    pub const ALL: [Scanner; 3] = [Scanner::Trivy, Scanner::Grype, Scanner::Scout];

    pub fn label(self) -> &'static str {
        match self {
            Scanner::Trivy => "Trivy",
            Scanner::Grype => "Grype",
            Scanner::Scout => "Docker Scout",
        }
    }

    pub fn tool(self) -> crate::devops::tools::Tool {
        use crate::devops::tools::Tool;
        match self {
            Scanner::Trivy => Tool::Trivy,
            Scanner::Grype => Tool::Grype,
            Scanner::Scout => Tool::Docker,
        }
    }

    /// The command that scans `image`, writing the report to `out`.
    pub fn spec(self, image: &str, out: &Path, cwd: &Path) -> ProcSpec {
        let out = out.to_string_lossy().into_owned();
        match self {
            Scanner::Trivy => ProcSpec::new("trivy", cwd).args([
                "image",
                "--scanners",
                "vuln",
                "--format",
                "json",
                "--output",
                &out,
                image,
            ]),
            Scanner::Grype => {
                ProcSpec::new("grype", cwd).args([image, "-o", "json", "--file", &out])
            }
            Scanner::Scout => ProcSpec::new("docker", cwd).args([
                "scout", "cves", "--format", "sarif", "--output", &out, image,
            ]),
        }
        .with_color()
    }

    /// Where a scan report goes: a fresh file in the temp directory.
    pub fn report_path(self, run: u64) -> PathBuf {
        std::env::temp_dir().join(format!(
            "hadion-scan-{}-{}-{run}.json",
            std::process::id(),
            match self {
                Scanner::Trivy => "trivy",
                Scanner::Grype => "grype",
                Scanner::Scout => "scout",
            }
        ))
    }

    pub fn parse(self, report: &[u8]) -> Result<ScanSummary> {
        match self {
            Scanner::Trivy => parse_trivy(report),
            Scanner::Grype => parse_grype(report),
            Scanner::Scout => parse_scout(report),
        }
    }
}

/// Did Docker Scout fail because nobody is logged in to Docker Hub?
pub fn scout_needs_login(output: &str) -> bool {
    let l = output.to_ascii_lowercase();
    l.contains("log in with your docker id")
        || l.contains("not logged in")
        || l.contains("docker login")
        || l.contains("authentication required")
        || l.contains("unauthorized")
}

/// Is `docker scout` available (the CLI plugin), given `docker scout version` output?
pub fn scout_version(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|l| l.trim().strip_prefix("version:"))
        .map(|v| v.split_whitespace().next().unwrap_or("").to_owned())
        .filter(|v| !v.is_empty())
}

/// One vulnerability of a scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageVuln {
    pub id: String,
    pub package: String,
    pub installed: String,
    pub fixed: Option<String>,
    pub severity: Severity,
    pub title: String,
    pub url: String,
}

/// A scan report, summarised.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanSummary {
    pub scanner: String,
    pub image: String,
    /// Critical, high, medium, low, unknown.
    pub counts: [usize; 5],
    pub total: usize,
    pub fixable: usize,
    /// Most severe first, fixable first, at most 30.
    pub top: Vec<ImageVuln>,
    /// `(target, vulnerabilities)` for scanners that report per layer / OS.
    pub targets: Vec<(String, usize)>,
}

fn summarise(
    scanner: &str,
    image: String,
    mut all: Vec<ImageVuln>,
    targets: Vec<(String, usize)>,
) -> ScanSummary {
    // The same CVE in the same package is one finding.
    all.sort_by(|a, b| {
        (a.id.as_str(), a.package.as_str()).cmp(&(b.id.as_str(), b.package.as_str()))
    });
    all.dedup_by(|a, b| a.id == b.id && a.package == b.package);
    let counts = super::alerts::severity_counts(all.iter().map(|v| v.severity));
    let fixable = all.iter().filter(|v| v.fixed.is_some()).count();
    let total = all.len();
    all.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then(b.fixed.is_some().cmp(&a.fixed.is_some()))
            .then(a.id.cmp(&b.id))
    });
    all.truncate(30);
    ScanSummary {
        scanner: scanner.to_owned(),
        image,
        counts,
        total,
        fixable,
        top: all,
        targets,
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct TrivyVuln {
    #[serde(rename = "VulnerabilityID")]
    id: String,
    #[serde(rename = "PkgName")]
    pkg: String,
    #[serde(rename = "InstalledVersion")]
    installed: String,
    #[serde(rename = "FixedVersion")]
    fixed: Option<String>,
    #[serde(rename = "Severity")]
    severity: String,
    #[serde(rename = "Title")]
    title: Option<String>,
    #[serde(rename = "PrimaryURL")]
    url: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct TrivyResult {
    #[serde(rename = "Target")]
    target: String,
    #[serde(rename = "Vulnerabilities")]
    vulns: Option<Vec<TrivyVuln>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct TrivyReport {
    #[serde(rename = "ArtifactName")]
    artifact: String,
    #[serde(rename = "Results")]
    results: Vec<TrivyResult>,
}

/// `trivy image --format json`.
pub fn parse_trivy(body: &[u8]) -> Result<ScanSummary> {
    let r: TrivyReport = serde_json::from_slice(body)
        .map_err(|e| crate::Error::new(format!("Trivy report: {e}")))?;
    let mut all = Vec::new();
    let mut targets = Vec::new();
    for res in r.results {
        let vulns = res.vulns.unwrap_or_default();
        targets.push((res.target, vulns.len()));
        for v in vulns {
            all.push(ImageVuln {
                id: v.id,
                package: v.pkg,
                installed: v.installed,
                fixed: v.fixed.filter(|f| !f.is_empty()),
                severity: Severity::parse(&v.severity),
                title: v.title.unwrap_or_default(),
                url: v.url.unwrap_or_default(),
            });
        }
    }
    Ok(summarise("Trivy", r.artifact, all, targets))
}

/// `grype IMG -o json`.
pub fn parse_grype(body: &[u8]) -> Result<ScanSummary> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|e| crate::Error::new(format!("Grype report: {e}")))?;
    let s = |v: &Value, path: &[&str]| -> String {
        let mut cur = v;
        for p in path {
            match cur.get(p) {
                Some(x) => cur = x,
                None => return String::new(),
            }
        }
        cur.as_str().unwrap_or("").to_owned()
    };
    let image = s(&v, &["source", "target", "userInput"]);
    let mut all = Vec::new();
    for m in v
        .get("matches")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let vuln = m.get("vulnerability").cloned().unwrap_or_default();
        let fixed = vuln
            .get("fix")
            .and_then(|f| f.get("versions"))
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            .map(str::to_owned);
        all.push(ImageVuln {
            id: s(&vuln, &["id"]),
            package: s(m, &["artifact", "name"]),
            installed: s(m, &["artifact", "version"]),
            fixed,
            severity: Severity::parse(&s(&vuln, &["severity"])),
            title: super::clip(&s(&vuln, &["description"]), 140),
            url: vuln
                .get("urls")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| s(&vuln, &["dataSource"])),
        });
    }
    Ok(summarise("Grype", image, all, Vec::new()))
}

/// `docker scout cves --format sarif`.
pub fn parse_scout(body: &[u8]) -> Result<ScanSummary> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|e| crate::Error::new(format!("Docker Scout report: {e}")))?;
    let mut all = Vec::new();
    let mut image = String::new();
    for run in v
        .get("runs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let rules: BTreeMap<String, &Value> = run
            .get("tool")
            .and_then(|t| t.get("driver"))
            .and_then(|d| d.get("rules"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|r| Some((r.get("id")?.as_str()?.to_owned(), r)))
                    .collect()
            })
            .unwrap_or_default();
        if image.is_empty() {
            image = run
                .get("automationDetails")
                .and_then(|a| a.get("id"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
        }
        for res in run
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let id = res
                .get("ruleId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let rule = rules.get(&id);
            let props = rule.and_then(|r| r.get("properties"));
            let prop = |k: &str| {
                props
                    .and_then(|p| p.get(k))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned()
            };
            let mut severity = Severity::parse(&prop("cvssV3_severity"));
            if severity == Severity::Unknown {
                severity = prop("security-severity")
                    .parse::<f64>()
                    .map(Severity::from_score)
                    .unwrap_or(Severity::Unknown);
            }
            let purl = props
                .and_then(|p| p.get("purls"))
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(Value::as_str)
                .unwrap_or("");
            let package = super::sbom::parse_purl(purl)
                .map(|(_, n, _)| n.rsplit('/').next().unwrap_or(&n).to_owned())
                .unwrap_or_default();
            let fixed = prop("fixed_version");
            all.push(ImageVuln {
                package,
                installed: prop("affected_version"),
                fixed: if fixed.is_empty() || fixed == "not fixed" {
                    None
                } else {
                    Some(fixed)
                },
                severity,
                title: rule
                    .and_then(|r| r.get("shortDescription"))
                    .and_then(|d| d.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                url: rule
                    .and_then(|r| r.get("helpUri"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                id,
            });
        }
    }
    Ok(summarise("Docker Scout", image, all, Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DF: &str = r#"# syntax=docker/dockerfile:1
ARG NODE_VERSION=20
FROM node AS deps
ENV NPM_TOKEN=abc123 \
    NODE_ENV=production
ARG GITHUB_API_KEY
WORKDIR /app
COPY . .
RUN npm ci
RUN apt-get update && apt-get install -y curl \
    # a comment inside the continuation
    git
RUN curl -fsSL https://example.com/install.sh | bash
RUN apk add git && sudo make install

FROM --platform=linux/amd64 ghcr.io/acme/base:1.2@sha256:0123abcd AS build
ADD https://example.com/tool.tar.gz /opt/
ADD --checksum=sha256:aa https://example.com/ok.tar.gz /opt/
COPY --from=deps /app /app
RUN <<EOF
apt-get update
apt-get install -y --no-install-recommends ca-certificates
rm -rf /var/lib/apt/lists/*
EOF

FROM build
EXPOSE 8080 22/tcp
USER root
CMD ["node", "server.js"]
"#;

    #[test]
    fn parses_stages_and_instructions() {
        let df = parse("Dockerfile", DF);
        assert_eq!(df.global_args, vec!["NODE_VERSION"]);
        assert_eq!(df.stages.len(), 3);
        let s0 = &df.stages[0];
        assert_eq!(s0.name.as_deref(), Some("deps"));
        assert_eq!(s0.image.repo, "node");
        assert_eq!(s0.image.tag, None);
        assert_eq!(s0.runs, 4);
        assert_eq!(
            s0.variables,
            vec![
                (4, "ENV".to_string(), "NPM_TOKEN".to_string()),
                (4, "ENV".to_string(), "NODE_ENV".to_string()),
                (6, "ARG".to_string(), "GITHUB_API_KEY".to_string()),
            ]
        );
        let s1 = &df.stages[1];
        assert_eq!(s1.platform.as_deref(), Some("linux/amd64"));
        assert_eq!(s1.image.repo, "ghcr.io/acme/base");
        assert_eq!(s1.image.tag.as_deref(), Some("1.2"));
        assert_eq!(s1.image.digest.as_deref(), Some("sha256:0123abcd"));
        assert_eq!(s1.copies.len(), 3);
        let s2 = &df.stages[2];
        assert!(s2.from_stage);
        assert_eq!(s2.exposes.len(), 2);
        assert_eq!(s2.final_user(), Some("root"));
        // Continuation joined, comment dropped.
        let apt = df
            .instructions
            .iter()
            .find(|i| i.args.contains("apt-get install -y curl"))
            .unwrap();
        assert!(apt.args.ends_with("git"));
        assert!(!apt.args.contains("comment"));
        assert_eq!(apt.line, 10);
        assert_eq!(apt.end_line, 12);
        // Heredoc body kept.
        let here = df
            .instructions
            .iter()
            .find(|i| i.args.starts_with("<<EOF"))
            .unwrap();
        assert!(here.args.contains("--no-install-recommends"));
        // No secret value anywhere in the parsed variables.
        assert!(!format!("{:?}", df.stages[0].variables).contains("abc123"));
    }

    fn rules(df: &str) -> Vec<(&'static str, usize)> {
        let mut v: Vec<(&'static str, usize)> = lint(&parse("Dockerfile", df))
            .into_iter()
            .map(|f| (f.rule, f.line))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn lint_rules() {
        let r = rules(DF);
        let has = |rule: &str, line: usize| r.iter().any(|(k, l)| *k == rule && *l == line);
        assert!(has("unpinned-tag", 3), "{r:?}");
        assert!(has("no-digest", 3));
        // Pinned by digest: no tag or digest finding for stage 2.
        assert!(
            !r.iter()
                .any(|(k, l)| *l == 16 && (*k == "no-digest" || *k == "unpinned-tag"))
        );
        assert!(has("secret-variable", 4)); // ENV NPM_TOKEN
        assert!(has("secret-variable", 6)); // ARG GITHUB_API_KEY
        assert!(has("copy-before-install", 8));
        assert!(has("apt-recommends", 10));
        assert!(has("apt-lists", 10));
        assert!(has("curl-pipe-sh", 13));
        assert!(has("apk-cache", 14));
        assert!(has("sudo", 14));
        assert!(has("add-url", 17));
        assert!(!has("add-url", 18)); // has --checksum
        // The heredoc RUN cleans up and skips recommends.
        assert!(!r.iter().any(|(k, l)| *l == 20 && k.starts_with("apt")));
        assert!(has("expose-ssh", 27));
        assert!(has("root-user", 28));
        assert!(has("no-healthcheck", 26));
        // NODE_ENV is not a secret.
        assert_eq!(r.iter().filter(|(k, _)| *k == "secret-variable").count(), 2);
    }

    #[test]
    fn clean_dockerfile() {
        let df = "FROM python:3.12-slim@sha256:abc\nWORKDIR /app\nCOPY requirements.txt .\nRUN pip install -r requirements.txt\nCOPY . .\nRUN useradd -r app\nUSER app\nHEALTHCHECK CMD curl -f http://localhost/ || exit 1\nCMD [\"python\", \"app.py\"]\n";
        assert!(rules(df).is_empty(), "{:?}", rules(df));
        let no_user = "FROM alpine:3.20\nRUN apk add --no-cache curl\n";
        let r = rules(no_user);
        assert!(r.contains(&("root-user", 1)));
        assert!(r.contains(&("no-healthcheck", 1)));
        assert!(r.contains(&("no-digest", 1)));
        assert!(!r.iter().any(|(k, _)| *k == "apk-cache"));
        let latest = "FROM nginx:latest\nUSER 101\nHEALTHCHECK NONE\n";
        assert!(rules(latest).contains(&("unpinned-tag", 1)));
        let arg = "ARG BASE=alpine\nFROM $BASE\nUSER 1000\nHEALTHCHECK NONE\n";
        assert!(rules(arg).is_empty(), "{:?}", rules(arg));
    }

    #[test]
    fn names_and_refs() {
        assert!(secret_name("NPM_TOKEN"));
        assert!(secret_name("DB_PASSWORD"));
        assert!(secret_name("AWS_SECRET_ACCESS_KEY"));
        assert!(secret_name("STRIPE_API_KEY"));
        assert!(!secret_name("NODE_ENV"));
        assert!(!secret_name("GPG_KEYS"));
        assert!(!secret_name("GPG_KEY"));
        assert!(!secret_name("PASSWORD_FILE"));
        assert!(!secret_name("AUTHOR"));
        assert_eq!(env_names(r#"A=1 B="two words" C=x=y"#), vec!["A", "B", "C"]);
        assert_eq!(env_names("PATH /usr/bin:$PATH"), vec!["PATH"]);
        assert_eq!(arg_names("A=1 B"), vec!["A", "B"]);
        let r = ImageRef::parse("localhost:5000/team/app:2.0");
        assert_eq!(r.repo, "localhost:5000/team/app");
        assert_eq!(r.tag.as_deref(), Some("2.0"));
        let r = ImageRef::parse("localhost:5000/team/app");
        assert_eq!(r.tag, None);
        assert!(is_dockerfile("deploy/docker/api-gateway.Dockerfile"));
        assert!(is_dockerfile("Dockerfile.dev"));
        assert!(is_dockerfile("x/Containerfile"));
        assert!(!is_dockerfile("docs/dockerfiles.md"));
        assert!(is_compose("deploy/compose/docker-compose.yml"));
        assert!(is_compose("compose.yaml"));
        assert!(is_compose("docker-compose.e2e.yml"));
        assert!(!is_compose("composer.json"));
    }

    #[test]
    fn compose_services() {
        let text = "services:\n  db:\n    image: postgres:16\n  api:\n    build:\n      context: .\n      dockerfile: deploy/api.Dockerfile\n  web:\n    image: ghcr.io/acme/web:${TAG}\n    build: ./web\n";
        let c = compose_images("docker-compose.yml", text);
        assert_eq!(c.len(), 3);
        assert_eq!(c[0].service, "db");
        assert_eq!(c[0].image.as_ref().unwrap().tag.as_deref(), Some("16"));
        assert_eq!(c[0].line, 3);
        assert_eq!(c[1].build.as_deref(), Some(". (deploy/api.Dockerfile)"));
        assert!(c[1].image.is_none());
        assert!(c[2].image.as_ref().unwrap().uses_arg());
        assert_eq!(c[2].build.as_deref(), Some("./web"));
    }

    const TRIVY: &str = r#"{"SchemaVersion": 2, "ArtifactName": "alpine:3.18", "ArtifactType": "container_image",
      "Results": [
        {"Target": "alpine:3.18 (alpine 3.18.4)", "Class": "os-pkgs", "Type": "alpine", "Vulnerabilities": [
          {"VulnerabilityID": "CVE-2023-5363", "PkgID": "libcrypto3@3.1.3-r0", "PkgName": "libcrypto3", "InstalledVersion": "3.1.3-r0",
           "FixedVersion": "3.1.4-r0", "Status": "fixed", "Severity": "HIGH", "Title": "openssl: Incorrect cipher key and IV length processing",
           "PrimaryURL": "https://avd.aquasec.com/nvd/cve-2023-5363"},
          {"VulnerabilityID": "CVE-2023-5363", "PkgName": "libssl3", "InstalledVersion": "3.1.3-r0", "FixedVersion": "3.1.4-r0", "Severity": "HIGH"},
          {"VulnerabilityID": "CVE-2023-5678", "PkgName": "libssl3", "InstalledVersion": "3.1.3-r0", "FixedVersion": "", "Severity": "MEDIUM"},
          {"VulnerabilityID": "CVE-2024-0001", "PkgName": "busybox", "InstalledVersion": "1.36.1-r2", "Severity": "CRITICAL"}
        ]},
        {"Target": "app/package-lock.json", "Class": "lang-pkgs", "Type": "npm", "Vulnerabilities": null}
      ]}"#;

    #[test]
    fn trivy_summary() {
        let s = parse_trivy(TRIVY.as_bytes()).unwrap();
        assert_eq!(s.image, "alpine:3.18");
        assert_eq!(s.total, 4);
        assert_eq!(s.counts, [1, 2, 1, 0, 0]);
        assert_eq!(s.fixable, 2);
        assert_eq!(s.top[0].id, "CVE-2024-0001");
        assert_eq!(s.top[1].fixed.as_deref(), Some("3.1.4-r0"));
        assert_eq!(s.targets.len(), 2);
        assert_eq!(s.targets[1].1, 0);
    }

    const GRYPE: &str = r#"{"matches": [
      {"vulnerability": {"id": "CVE-2023-5363", "dataSource": "https://nvd.nist.gov/vuln/detail/CVE-2023-5363", "severity": "High",
        "urls": ["https://www.openssl.org/news/secadv/20231024.txt"], "description": "Issue summary: A bug has been identified",
        "fix": {"versions": ["3.1.4-r0"], "state": "fixed"}},
       "artifact": {"name": "libcrypto3", "version": "3.1.3-r0", "type": "apk"}},
      {"vulnerability": {"id": "GHSA-xxxx", "severity": "Negligible", "fix": {"versions": [], "state": "not-fixed"}},
       "artifact": {"name": "lodash", "version": "4.17.15", "type": "npm"}}],
      "source": {"type": "image", "target": {"userInput": "alpine:3.18"}},
      "descriptor": {"name": "grype", "version": "0.74.0"}}"#;

    #[test]
    fn grype_summary() {
        let s = parse_grype(GRYPE.as_bytes()).unwrap();
        assert_eq!(s.image, "alpine:3.18");
        assert_eq!(s.total, 2);
        assert_eq!(s.counts, [0, 1, 0, 1, 0]);
        assert_eq!(s.fixable, 1);
        assert_eq!(
            s.top[0].url,
            "https://www.openssl.org/news/secadv/20231024.txt"
        );
        assert_eq!(s.top[0].package, "libcrypto3");
    }

    const SCOUT: &str = r#"{"version": "2.1.0", "runs": [{"tool": {"driver": {"name": "docker scout", "rules": [
      {"id": "CVE-2023-5363", "name": "OsPackageVulnerability", "shortDescription": {"text": "CVE-2023-5363"},
       "helpUri": "https://scout.docker.com/v/CVE-2023-5363",
       "properties": {"affected_version": "3.1.3-r0", "cvssV3_severity": "HIGH", "fixed_version": "3.1.4-r0",
         "purls": ["pkg:apk/alpine/libcrypto3@3.1.3-r0?os_name=alpine&os_version=3.18"], "security-severity": "7.5"}},
      {"id": "CVE-2020-1", "properties": {"affected_version": "1.0", "fixed_version": "not fixed", "security-severity": "9.8",
         "purls": ["pkg:npm/lodash@1.0"]}}]}},
      "results": [{"ruleId": "CVE-2023-5363", "level": "error"}, {"ruleId": "CVE-2020-1"}]}]}"#;

    #[test]
    fn scout_summary_and_login() {
        let s = parse_scout(SCOUT.as_bytes()).unwrap();
        assert_eq!(s.total, 2);
        assert_eq!(s.counts, [1, 1, 0, 0, 0]);
        assert_eq!(s.fixable, 1);
        let lib = s.top.iter().find(|v| v.id == "CVE-2023-5363").unwrap();
        assert_eq!(lib.package, "libcrypto3");
        assert_eq!(lib.fixed.as_deref(), Some("3.1.4-r0"));
        assert!(scout_needs_login(
            "Log in with your Docker ID to use Docker Scout"
        ));
        assert!(!scout_needs_login("Analyzing image alpine"));
        assert_eq!(
            scout_version("art\n\nversion: v1.24.0 (go1.26.5 - windows/amd64)\ngit commit: b1c9"),
            Some("v1.24.0".into())
        );
        let spec = Scanner::Trivy.spec("alpine:3.20", Path::new("/tmp/out.json"), Path::new("."));
        assert_eq!(spec.program, "trivy");
        assert!(spec.args.contains(&"--output".to_string()));
        assert_eq!(spec.args.last().map(String::as_str), Some("alpine:3.20"));
        let spec = Scanner::Scout.spec("alpine:3.20", Path::new("o.json"), Path::new("."));
        assert_eq!(&spec.args[..2], &["scout".to_string(), "cves".to_string()]);
    }
}
