//! Running workflow jobs locally with `act` (nektos/act, needs Docker):
//! parsing `act -l` and building the command line of a run.
//!
//! Secrets are only ever passed as a file path (`--secret-file`); their
//! values never reach a command line.

use std::path::{Path, PathBuf};

use crate::devops::proc::ProcSpec;

/// One row of `act -l`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActJob {
    pub stage: u32,
    pub job_id: String,
    pub job_name: String,
    pub workflow_name: String,
    pub workflow_file: String,
    pub events: Vec<String>,
}

/// `act -l` (lists every job of every workflow with its stage and events).
pub fn list_spec(workdir: &Path) -> ProcSpec {
    ProcSpec::new("act", workdir).arg("-l")
}

/// Parse `act -l`. The table is aligned with spaces under a header
/// (`Stage  Job ID  Job name  Workflow name  Workflow file  Events`); log
/// lines before it (`INFO[0000] …`, warnings) are skipped. Columns are cut
/// at the header's offsets, so names with single spaces survive.
pub fn parse_list(text: &str) -> Vec<ActJob> {
    let lines: Vec<&str> = text.lines().collect();
    let Some(h) = lines
        .iter()
        .position(|l| l.contains("Job ID") && l.contains("Stage"))
    else {
        return Vec::new();
    };
    let header = lines[h];
    let names = [
        "Stage",
        "Job ID",
        "Job name",
        "Workflow name",
        "Workflow file",
        "Events",
    ];
    let mut cols: Vec<(usize, &str)> = names
        .iter()
        .filter_map(|n| header.find(n).map(|i| (i, *n)))
        .collect();
    cols.sort_by_key(|c| c.0);
    let col = |line: &str, name: &str| -> String {
        let Some(pos) = cols.iter().position(|c| c.1 == name) else {
            return String::new();
        };
        let start = cols[pos].0;
        let end = cols.get(pos + 1).map(|c| c.0).unwrap_or(usize::MAX);
        let chars: Vec<char> = line.chars().collect();
        if start >= chars.len() {
            return String::new();
        }
        chars[start..end.min(chars.len())]
            .iter()
            .collect::<String>()
            .trim()
            .to_owned()
    };
    lines[h + 1..]
        .iter()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            let stage: u32 = col(l, "Stage").parse().ok()?;
            let job_id = col(l, "Job ID");
            if job_id.is_empty() {
                return None;
            }
            Some(ActJob {
                stage,
                job_name: col(l, "Job name"),
                workflow_name: col(l, "Workflow name"),
                workflow_file: col(l, "Workflow file"),
                events: col(l, "Events")
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect(),
                job_id,
            })
        })
        .collect()
}

/// Default image for `ubuntu-latest` (the medium catthehacker image act
/// suggests on first run).
pub const DEFAULT_IMAGE: &str = "catthehacker/ubuntu:act-latest";

/// What to run with `act`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActRun {
    /// `push`, `pull_request`, `workflow_dispatch`, …
    pub event: String,
    pub job: String,
    /// Workflow file relative to the repository (`.github/workflows/ci.yml`).
    pub workflow_file: String,
    /// `--input name=value` for `workflow_dispatch` inputs.
    pub inputs: Vec<(String, String)>,
    /// `-P label=image`, e.g. `ubuntu-latest` → `catthehacker/ubuntu:act-latest`.
    pub platform: Option<(String, String)>,
    /// `--secret-file PATH` (a dotenv file; the values stay in the file).
    pub secret_file: Option<PathBuf>,
}

impl ActRun {
    /// The arguments after `act`.
    pub fn args(&self) -> Vec<String> {
        let mut a = Vec::new();
        if !self.event.trim().is_empty() {
            a.push(self.event.trim().to_owned());
        }
        if !self.job.trim().is_empty() {
            a.push("-j".to_owned());
            a.push(self.job.trim().to_owned());
        }
        if !self.workflow_file.trim().is_empty() {
            a.push("-W".to_owned());
            a.push(self.workflow_file.trim().to_owned());
        }
        for (k, v) in &self.inputs {
            if k.trim().is_empty() {
                continue;
            }
            a.push("--input".to_owned());
            a.push(format!("{}={v}", k.trim()));
        }
        if let Some((label, image)) = &self.platform
            && !label.trim().is_empty()
            && !image.trim().is_empty()
        {
            a.push("-P".to_owned());
            a.push(format!("{}={}", label.trim(), image.trim()));
        }
        if let Some(p) = &self.secret_file {
            a.push("--secret-file".to_owned());
            a.push(p.display().to_string());
        }
        a
    }

    /// The process to stream into the console.
    pub fn spec(&self, workdir: &Path) -> ProcSpec {
        ProcSpec::new("act", workdir).args(self.args()).with_color()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = "INFO[0000] Using docker host 'npipe:////./pipe/docker_engine', and daemon socket 'npipe:////./pipe/docker_engine'
WARN  ⚠ You are using Apple M-series chip and you have not specified container architecture, you might encounter issues while running act. If so, try running it with '--container-architecture linux/amd64'. ⚠
Stage  Job ID     Job name          Workflow name   Workflow file  Events
0      lint       Lint the code     CI checks       ci.yml         push,pull_request
0      build      build             CI checks       ci.yml         push,pull_request
1      deploy     Deploy to prod    Deploy          deploy.yml     workflow_dispatch
";

    #[test]
    fn parses_act_list() {
        let jobs = parse_list(LIST);
        assert_eq!(jobs.len(), 3);
        assert_eq!(jobs[0].job_id, "lint");
        assert_eq!(jobs[0].job_name, "Lint the code");
        assert_eq!(jobs[0].workflow_name, "CI checks");
        assert_eq!(jobs[0].workflow_file, "ci.yml");
        assert_eq!(jobs[0].events, ["push", "pull_request"]);
        assert_eq!(jobs[2].stage, 1);
        assert_eq!(jobs[2].job_name, "Deploy to prod");
        assert_eq!(jobs[2].events, ["workflow_dispatch"]);
        assert!(parse_list("no table here").is_empty());
        assert!(
            parse_list("Stage  Job ID  Job name  Workflow name  Workflow file  Events\n")
                .is_empty()
        );
    }

    #[test]
    fn builds_the_command() {
        let run = ActRun {
            event: "workflow_dispatch".into(),
            job: "deploy".into(),
            workflow_file: ".github/workflows/deploy.yml".into(),
            inputs: vec![
                ("level".into(), "debug".into()),
                ("note".into(), "a b".into()),
            ],
            platform: Some(("ubuntu-latest".into(), DEFAULT_IMAGE.into())),
            secret_file: Some(PathBuf::from("C:/x/.secrets")),
        };
        assert_eq!(
            run.args(),
            [
                "workflow_dispatch",
                "-j",
                "deploy",
                "-W",
                ".github/workflows/deploy.yml",
                "--input",
                "level=debug",
                "--input",
                "note=a b",
                "-P",
                "ubuntu-latest=catthehacker/ubuntu:act-latest",
                "--secret-file",
                "C:/x/.secrets",
            ]
        );
        let spec = run.spec(Path::new("."));
        assert_eq!(spec.program, "act");
        assert!(spec.display().contains("\"note=a b\""));
        let minimal = ActRun {
            event: "push".into(),
            job: "build".into(),
            ..Default::default()
        };
        assert_eq!(minimal.args(), ["push", "-j", "build"]);
        assert_eq!(list_spec(Path::new(".")).args, ["-l"]);
    }
}
