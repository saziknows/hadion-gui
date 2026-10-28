//! Fetching a release and staging it: download (`gh release download`, or a
//! copy from the test folder), verify the SHA-256 against `SHA256SUMS` (and
//! GitHub's asset digest when there is one), extract the executable from
//! the zip, stage it as `<exe>.new`. Runs on a background thread and
//! reports progress through a shared [`Progress`].

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::checksum;
use super::release::{Asset, Platform, SUMS, is_zip};
use super::swap::{self, Paths};
use super::zip;
use crate::devops::proc::{self, ProcSpec};

/// Where releases come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// `owner/repo` on GitHub, through the user's `gh`.
    GitHub(String),
    /// `GITVIS_UPDATE_DIR`: `releases.json` and the asset files.
    Folder(PathBuf),
}

impl Source {
    pub fn label(&self) -> String {
        match self {
            Source::GitHub(r) => r.clone(),
            Source::Folder(_) => "the test folder (GITVIS_UPDATE_DIR)".to_owned(),
        }
    }
}

/// A stage of the pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Download,
    Verify,
    Extract,
    Stage,
}

impl Step {
    pub const ALL: [Step; 4] = [Step::Download, Step::Verify, Step::Extract, Step::Stage];

    pub fn label(self) -> &'static str {
        match self {
            Step::Download => "Download",
            Step::Verify => "Verify SHA-256",
            Step::Extract => "Extract",
            Step::Stage => "Stage",
        }
    }
}

/// One finished step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepDone {
    pub step: Step,
    pub detail: String,
    pub ms: u64,
}

/// Live state of a running job, shared with the UI.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub step: Option<Step>,
    /// The command (or copy) that downloads.
    pub command: String,
    /// Output lines of the download command (last 200).
    pub lines: Vec<String>,
    /// Bytes on disk so far, and the expected total (asset + sums).
    pub bytes: u64,
    pub total: u64,
    pub done: Vec<StepDone>,
}

/// Everything a job needs.
#[derive(Clone, Debug)]
pub struct Job {
    pub source: Source,
    pub tag: String,
    pub version: String,
    pub asset: Asset,
    pub sums: Asset,
    /// Download folder (emptied first).
    pub dir: PathBuf,
    /// The running executable.
    pub exe: PathBuf,
    pub platform: Platform,
    /// Test mode only: copy at most this many KiB per second (to see the
    /// progress); 0 = no limit.
    pub throttle_kib: u64,
}

/// A staged update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Staged {
    pub tag: String,
    pub version: String,
    pub new_exe: PathBuf,
    pub sha256: String,
    pub bytes: u64,
    pub steps: Vec<StepDone>,
}

/// Where a job stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub step: Step,
    pub error: String,
    pub steps: Vec<StepDone>,
}

/// `gh release download TAG -R O/R -p ASSET -p SHA256SUMS -D DIR`.
pub fn gh_download_args(tag: &str, repo: &str, asset: &str, dir: &Path) -> Vec<String> {
    vec![
        "release".into(),
        "download".into(),
        tag.into(),
        "-R".into(),
        repo.into(),
        "-p".into(),
        asset.into(),
        "-p".into(),
        SUMS.into(),
        "-D".into(),
        dir.display().to_string(),
    ]
}

/// The download command as shown to the user.
pub fn download_command(job: &Job) -> String {
    match &job.source {
        Source::GitHub(repo) => {
            let args = gh_download_args(&job.tag, repo, &job.asset.name, &job.dir);
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            format!("gh {}", proc::render_args(&refs))
        }
        Source::Folder(src) => format!(
            "copy {} and {} from {} to {}",
            job.asset.name,
            SUMS,
            src.display(),
            job.dir.display()
        ),
    }
}

fn set(progress: &Mutex<Progress>, f: impl FnOnce(&mut Progress)) {
    if let Ok(mut p) = progress.lock() {
        f(&mut p);
    }
}

fn dir_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter_map(|e| e.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

/// Run the whole job. `commands` receives every command line it ran.
pub fn run(
    job: &Job,
    progress: &Mutex<Progress>,
    cancel: &AtomicBool,
    commands: &mut Vec<String>,
) -> Result<Staged, Failure> {
    let mut steps: Vec<StepDone> = Vec::new();
    let fail = |step: Step, error: String, steps: &[StepDone]| Failure {
        step,
        error,
        steps: steps.to_vec(),
    };
    let finish = |step: Step, detail: String, started: Instant, steps: &mut Vec<StepDone>| {
        let d = StepDone {
            step,
            detail,
            ms: started.elapsed().as_millis() as u64,
        };
        steps.push(d.clone());
        set(progress, |p| p.done.push(d));
    };

    // 1. Download.
    let t = Instant::now();
    let command = download_command(job);
    set(progress, |p| {
        p.step = Some(Step::Download);
        p.command = command.clone();
        p.total = job.asset.size;
    });
    commands.push(command);
    if let Err(e) = std::fs::remove_dir_all(&job.dir)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        return Err(fail(
            Step::Download,
            format!("Could not empty {}: {e}", job.dir.display()),
            &steps,
        ));
    }
    std::fs::create_dir_all(&job.dir).map_err(|e| {
        fail(
            Step::Download,
            format!("Could not create {}: {e}", job.dir.display()),
            &steps,
        )
    })?;
    let downloaded = match &job.source {
        Source::GitHub(repo) => gh_download(job, repo, progress, cancel),
        Source::Folder(src) => copy_from_folder(job, src, progress, cancel),
    };
    if let Err(e) = downloaded {
        return Err(fail(Step::Download, e, &steps));
    }
    let asset_path = job.dir.join(&job.asset.name);
    let sums_path = job.dir.join(SUMS);
    for p in [&asset_path, &sums_path] {
        if !p.is_file() {
            return Err(fail(
                Step::Download,
                format!(
                    "{} was not downloaded",
                    p.file_name().unwrap_or_default().to_string_lossy()
                ),
                &steps,
            ));
        }
    }
    let size = std::fs::metadata(&asset_path).map(|m| m.len()).unwrap_or(0);
    if job.asset.size > 0 && size != job.asset.size {
        return Err(fail(
            Step::Download,
            format!(
                "{} is {size} bytes, the release says {}",
                job.asset.name, job.asset.size
            ),
            &steps,
        ));
    }
    finish(
        Step::Download,
        format!(
            "{} ({}) and {SUMS} in {}",
            job.asset.name,
            crate::devops::plural_bytes(size as usize),
            job.dir.display()
        ),
        t,
        &mut steps,
    );

    // 2. Verify.
    let t = Instant::now();
    set(progress, |p| p.step = Some(Step::Verify));
    let sums = std::fs::read_to_string(&sums_path)
        .map_err(|e| fail(Step::Verify, format!("Could not read {SUMS}: {e}"), &steps))?;
    let actual = checksum::sha256_file(&asset_path).map_err(|e| {
        fail(
            Step::Verify,
            format!("Could not read {}: {e}", job.asset.name),
            &steps,
        )
    })?;
    let how = checksum::verify(&sums, &job.asset.name, &actual, job.asset.digest.as_deref())
        .map_err(|e| fail(Step::Verify, e, &steps))?;
    finish(Step::Verify, format!("{actual} {how}"), t, &mut steps);
    if cancel.load(Ordering::Relaxed) {
        return Err(fail(Step::Extract, "Cancelled".into(), &steps));
    }

    // 3. Extract.
    let t = Instant::now();
    set(progress, |p| p.step = Some(Step::Extract));
    let exe_name = job.platform.exe_name();
    let (exe_path, detail) = if is_zip(&job.asset.name) {
        let data = std::fs::read(&asset_path).map_err(|e| {
            fail(
                Step::Extract,
                format!("Could not read {}: {e}", job.asset.name),
                &steps,
            )
        })?;
        let entries = zip::entries(&data).map_err(|e| fail(Step::Extract, e, &steps))?;
        let entry = zip::find(&entries, exe_name).ok_or_else(|| {
            fail(
                Step::Extract,
                format!(
                    "{} has no {exe_name} (it holds {})",
                    job.asset.name,
                    entries
                        .iter()
                        .take(12)
                        .map(|e| e.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                &steps,
            )
        })?;
        let bytes = zip::read(&data, entry).map_err(|e| fail(Step::Extract, e, &steps))?;
        let out = job.dir.join(exe_name);
        std::fs::write(&out, &bytes).map_err(|e| {
            fail(
                Step::Extract,
                format!("Could not write {}: {e}", out.display()),
                &steps,
            )
        })?;
        (
            out,
            format!(
                "{} ({}, CRC-32 checked)",
                entry.name,
                crate::devops::plural_bytes(bytes.len())
            ),
        )
    } else {
        (
            asset_path.clone(),
            format!("{} is the executable itself", job.asset.name),
        )
    };
    if let Err(e) = check_executable(&exe_path, &job.platform) {
        return Err(fail(Step::Extract, e, &steps));
    }
    finish(Step::Extract, detail, t, &mut steps);

    // 4. Stage next to the running executable.
    let t = Instant::now();
    set(progress, |p| p.step = Some(Step::Stage));
    let paths = Paths::for_exe(&job.exe);
    swap::writable_dir(&job.exe).map_err(|e| fail(Step::Stage, e, &steps))?;
    let bytes = swap::stage(&exe_path, &paths).map_err(|e| fail(Step::Stage, e, &steps))?;
    let staged_sha = checksum::sha256_file(&paths.new).map_err(|e| {
        fail(
            Step::Stage,
            format!("Could not read back the staged file: {e}"),
            &steps,
        )
    })?;
    let source_sha = checksum::sha256_file(&exe_path).unwrap_or_default();
    if staged_sha != source_sha {
        let _ = swap::discard(&paths);
        return Err(fail(
            Step::Stage,
            "The staged copy differs from the verified file".into(),
            &steps,
        ));
    }
    finish(
        Step::Stage,
        format!("{}", paths.new.display()),
        t,
        &mut steps,
    );
    set(progress, |p| p.step = None);
    Ok(Staged {
        tag: job.tag.clone(),
        version: job.version.clone(),
        new_exe: paths.new,
        sha256: staged_sha,
        bytes,
        steps,
    })
}

/// A Windows executable starts with `MZ`, an ELF one with `\x7fELF`, a
/// Mach-O one with its magic numbers.
pub fn check_executable(path: &Path, platform: &Platform) -> Result<(), String> {
    let mut head = [0u8; 4];
    let n = std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    let ok = match platform.os.as_str() {
        "windows" => n >= 2 && head[..2] == *b"MZ",
        "linux" => n == 4 && head == *b"\x7fELF",
        "macos" => {
            n == 4
                && matches!(
                    head,
                    [0xcf, 0xfa, 0xed, 0xfe] | [0xca, 0xfe, 0xba, 0xbe] | [0xfe, 0xed, 0xfa, 0xcf]
                )
        }
        _ => true,
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "{} is not a {} executable",
            path.file_name().unwrap_or_default().to_string_lossy(),
            platform.os
        ))
    }
}

fn gh_download(
    job: &Job,
    repo: &str,
    progress: &Mutex<Progress>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let spec = ProcSpec::new(crate::github::gh_program(), &job.dir).args(gh_download_args(
        &job.tag,
        repo,
        &job.asset.name,
        &job.dir,
    ));
    let finished = AtomicBool::new(false);
    let result = std::thread::scope(|s| {
        // Watch the folder fill up while gh runs.
        s.spawn(|| {
            while !finished.load(Ordering::Relaxed) {
                let b = dir_bytes(&job.dir);
                set(progress, |p| p.bytes = b);
                std::thread::sleep(Duration::from_millis(200));
            }
        });
        let r = proc::run_streaming(
            &spec,
            cancel,
            |_| {},
            |lines| {
                set(progress, |p| {
                    p.lines.extend(lines.into_iter().map(|l| l.text));
                    let n = p.lines.len();
                    if n > 200 {
                        p.lines.drain(..n - 200);
                    }
                })
            },
        );
        finished.store(true, Ordering::Relaxed);
        r
    });
    set(progress, |p| p.bytes = dir_bytes(&job.dir));
    let exit = result.map_err(|e| e.to_string())?;
    if exit.cancelled {
        return Err("Cancelled".into());
    }
    if !exit.success() {
        let tail = progress
            .lock()
            .map(|p| {
                p.lines
                    .iter()
                    .rev()
                    .take(6)
                    .rev()
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
            .join("\n");
        return Err(format!(
            "gh exited with {}{}",
            exit.code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "no code".into()),
            if tail.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", tail.trim())
            }
        ));
    }
    Ok(())
}

fn copy_from_folder(
    job: &Job,
    src: &Path,
    progress: &Mutex<Progress>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut done = 0u64;
    for name in [job.asset.name.as_str(), SUMS] {
        let from = src.join(name);
        let to = job.dir.join(name);
        let mut r = std::fs::File::open(&from)
            .map_err(|e| format!("Could not open {}: {e}", from.display()))?;
        let mut w = std::fs::File::create(&to)
            .map_err(|e| format!("Could not create {}: {e}", to.display()))?;
        let mut buf = vec![0u8; 256 * 1024];
        let started = Instant::now();
        let mut copied = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                drop(w);
                let _ = std::fs::remove_file(&to);
                return Err("Cancelled".into());
            }
            let n = r
                .read(&mut buf)
                .map_err(|e| format!("Could not read {}: {e}", from.display()))?;
            if n == 0 {
                break;
            }
            w.write_all(&buf[..n])
                .map_err(|e| format!("Could not write {}: {e}", to.display()))?;
            copied += n as u64;
            done += n as u64;
            set(progress, |p| p.bytes = done);
            if job.throttle_kib > 0 {
                let due =
                    Duration::from_secs_f64(copied as f64 / (job.throttle_kib as f64 * 1024.0));
                if let Some(wait) = due.checked_sub(started.elapsed()) {
                    std::thread::sleep(wait.min(Duration::from_millis(500)));
                }
            }
        }
        set(progress, |p| p.lines.push(format!("copied {name}")));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Update log
// ---------------------------------------------------------------------------

/// Append `event` lines (`2026-10-27T21:14:03Z  download  …`) to the log.
pub fn log(path: &Path, event: &str, detail: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let detail = detail.replace(['\r', '\n'], " ");
        let _ = writeln!(
            f,
            "{}  {event:<9} {detail}",
            super::time::iso(super::time::now_secs())
        );
    }
}

/// The lines of the most recent update attempt: from its last `download`
/// (or, without one, its last `check`) to the end.
pub fn last_attempt(log_text: &str) -> Vec<String> {
    let lines: Vec<&str> = log_text.lines().filter(|l| !l.trim().is_empty()).collect();
    let is = |l: &str, ev: &str| l.split_whitespace().nth(1) == Some(ev);
    let start = lines
        .iter()
        .rposition(|l| is(l, "download"))
        .or_else(|| lines.iter().rposition(|l| is(l, "check")))
        .unwrap_or(0);
    lines[start..].iter().map(|l| (*l).to_owned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devops::packaging::zip::testing;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gitvis-update-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn job(root: &Path, asset: &str, size: u64) -> Job {
        let install = root.join("install");
        std::fs::create_dir_all(&install).unwrap();
        let exe = install.join("gitvis.exe");
        std::fs::write(&exe, b"MZ old").unwrap();
        Job {
            source: Source::Folder(root.join("releases")),
            tag: "v0.2.0".into(),
            version: "0.2.0".into(),
            asset: Asset {
                name: asset.into(),
                size,
                ..Default::default()
            },
            sums: Asset {
                name: SUMS.into(),
                ..Default::default()
            },
            dir: root.join("download"),
            exe,
            platform: Platform::new("windows", "x86_64"),
            throttle_kib: 0,
        }
    }

    #[test]
    fn gh_command_line() {
        let args = gh_download_args(
            "v0.2.0",
            "o/r",
            "gitvis-0.2.0-windows-x86_64.zip",
            Path::new("C:/t/d"),
        );
        assert_eq!(
            args.join(" "),
            "release download v0.2.0 -R o/r -p gitvis-0.2.0-windows-x86_64.zip -p SHA256SUMS -D C:/t/d"
        );
        // A read: allowed in safe mode.
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        assert_eq!(crate::devops::safety::remote_write("gh", &refs, None), None);
    }

    #[test]
    fn zip_release_end_to_end_from_a_folder() {
        let root = scratch("zip");
        let releases = root.join("releases");
        std::fs::create_dir_all(&releases).unwrap();
        let exe_bytes = b"MZ new version of gitvis".repeat(1000);
        let zip = testing::build(&[
            ("gitvis-0.2.0\\README.md", b"readme", true),
            ("gitvis-0.2.0\\gitvis.exe", &exe_bytes, true),
        ]);
        let name = "gitvis-0.2.0-windows-x86_64.zip";
        std::fs::write(releases.join(name), &zip).unwrap();
        std::fs::write(
            releases.join(SUMS),
            format!("{}  {name}\n", checksum::sha256_bytes(&zip)),
        )
        .unwrap();
        let j = job(&root, name, zip.len() as u64);
        let progress = Mutex::new(Progress::default());
        let mut commands = Vec::new();
        let staged = run(&j, &progress, &AtomicBool::new(false), &mut commands).unwrap();
        assert_eq!(std::fs::read(&staged.new_exe).unwrap(), exe_bytes);
        assert_eq!(staged.new_exe, root.join("install").join("gitvis.exe.new"));
        assert_eq!(staged.steps.len(), 4);
        assert!(staged.steps[1].detail.contains("matches SHA256SUMS"));
        assert!(staged.steps[2].detail.contains("CRC-32"));
        assert!(commands[0].starts_with("copy gitvis-0.2.0-windows-x86_64.zip"));
        let p = progress.lock().unwrap();
        assert_eq!(p.step, None);
        assert_eq!(
            p.bytes,
            zip.len() as u64 + std::fs::metadata(releases.join(SUMS)).unwrap().len()
        );
        // The running executable is untouched.
        assert_eq!(std::fs::read(&j.exe).unwrap(), b"MZ old");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn checksum_mismatch_stops_before_staging() {
        let root = scratch("bad");
        let releases = root.join("releases");
        std::fs::create_dir_all(&releases).unwrap();
        let name = "gitvis-0.2.0-windows-x86_64.exe";
        std::fs::write(releases.join(name), b"MZ tampered").unwrap();
        std::fs::write(
            releases.join(SUMS),
            format!("{}  {name}\n", checksum::sha256_bytes(b"MZ original")),
        )
        .unwrap();
        let j = job(&root, name, 0);
        let progress = Mutex::new(Progress::default());
        let f = run(&j, &progress, &AtomicBool::new(false), &mut Vec::new()).unwrap_err();
        assert_eq!(f.step, Step::Verify);
        assert!(f.error.contains("mismatch"), "{}", f.error);
        assert_eq!(f.steps.len(), 1);
        assert!(!Paths::for_exe(&j.exe).staged());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_asset_and_non_executables_fail_clearly() {
        let root = scratch("missing");
        std::fs::create_dir_all(root.join("releases")).unwrap();
        let j = job(&root, "gitvis-0.2.0-windows-x86_64.exe", 0);
        let f = run(
            &j,
            &Mutex::new(Progress::default()),
            &AtomicBool::new(false),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert_eq!(f.step, Step::Download);
        assert!(f.error.contains("Could not open"), "{}", f.error);
        let txt = root.join("x.exe");
        std::fs::write(&txt, b"#!/bin/sh").unwrap();
        assert!(check_executable(&txt, &Platform::new("windows", "x86_64")).is_err());
        assert!(check_executable(&txt, &Platform::new("linux", "x86_64")).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn log_and_last_attempt() {
        let root = scratch("log");
        let path = root.join("update.log");
        log(&path, "check", "o/r: v0.2.0 available");
        log(&path, "download", "v0.1.9 failed");
        log(&path, "check", "o/r: v0.2.0 available");
        log(&path, "download", "v0.2.0");
        log(&path, "stage", "C:/x/gitvis.exe.new\nsecond line");
        let text = std::fs::read_to_string(&path).unwrap();
        let last = last_attempt(&text);
        assert_eq!(last.len(), 2, "{last:?}");
        assert!(last[0].contains("download  v0.2.0"), "{}", last[0]);
        assert!(last[1].ends_with("C:/x/gitvis.exe.new second line"));
        assert_eq!(last_attempt("2026-01-01T00:00:00Z  check     x\n").len(), 1);
        assert!(last_attempt("").is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
