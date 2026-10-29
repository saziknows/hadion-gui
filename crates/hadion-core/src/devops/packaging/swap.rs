//! Replacing the running executable.
//!
//! Windows cannot overwrite or delete a running `.exe`, but it can rename
//! one. So an update is staged as `<exe>.new` next to the running file and
//! installed on restart:
//!
//! 1. remove a leftover `<exe>.old`;
//! 2. rename `<exe>` → `<exe>.old` (the running process keeps its image);
//! 3. rename `<exe>.new` → `<exe>`;
//! 4. start `<exe>` with the same arguments, then quit;
//! 5. the new process deletes `<exe>.old` once the old one has exited.
//!
//! Any failure in 2–4 renames everything back, so the old version stays in
//! place and the staged one stays staged.

use std::path::{Path, PathBuf};

/// The three files of an update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    pub exe: PathBuf,
    /// The staged new version.
    pub new: PathBuf,
    /// The previous version, kept until the new one has started.
    pub old: PathBuf,
}

fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

impl Paths {
    pub fn for_exe(exe: &Path) -> Self {
        Self {
            exe: exe.to_path_buf(),
            new: with_suffix(exe, ".new"),
            old: with_suffix(exe, ".old"),
        }
    }

    /// An update is staged.
    pub fn staged(&self) -> bool {
        self.new.is_file()
    }

    /// The steps of [`install`], as the confirmation shows them.
    pub fn plan(&self, args: &[String]) -> Vec<String> {
        let mut out = Vec::new();
        if self.old.exists() {
            out.push(format!("delete {}", self.old.display()));
        }
        out.push(format!(
            "rename {} → {}",
            self.exe.display(),
            file_name(&self.old)
        ));
        out.push(format!(
            "rename {} → {}",
            self.new.display(),
            file_name(&self.exe)
        ));
        let mut start = format!("start {}", self.exe.display());
        for a in args {
            start.push(' ');
            start.push_str(&crate::devops::proc::render_args(&[a.as_str()]));
        }
        out.push(start);
        out.push("close this window (settings are saved as usual)".to_owned());
        out.push(format!(
            "the new version deletes {} once this one has exited",
            file_name(&self.old)
        ));
        out
    }
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// Why the executable must not replace itself: it lives in a Cargo target
/// folder (the next `cargo build` would overwrite it, and the owner's
/// working copies run from there).
pub fn dev_build_reason(exe: &Path) -> Option<String> {
    let dir = exe.parent()?;
    let profile_dir = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let cargo_dir = dir.join(".fingerprint").is_dir()
        || dir.join(".cargo-lock").exists()
        || (matches!(profile_dir.as_str(), "debug" | "release")
            && dir
                .parent()
                .is_some_and(|t| t.join("CACHEDIR.TAG").exists()));
    cargo_dir.then(|| {
        format!(
            "{} is a development build inside a Cargo target folder ({}); the next cargo build would replace it. Install a release with the installer or the zip to use updates.",
            file_name(exe),
            dir.display()
        )
    })
}

/// Can files be created next to the executable?
pub fn writable_dir(exe: &Path) -> Result<(), String> {
    let dir = exe
        .parent()
        .ok_or_else(|| format!("{} has no folder", exe.display()))?;
    let probe = dir.join(format!(".hadion-write-test-{}", std::process::id()));
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => Err(format!(
            "Cannot write in {} ({e}). Install Hadion per user (the installer uses %LOCALAPPDATA%\\Programs\\hadion) to update it from here.",
            dir.display()
        )),
    }
}

/// Copy the verified executable to `<exe>.new` (through a temporary file,
/// so a half-copied file is never taken for a staged update).
pub fn stage(src: &Path, p: &Paths) -> Result<u64, String> {
    let tmp = with_suffix(&p.exe, ".new.part");
    let _ = std::fs::remove_file(&tmp);
    let n = std::fs::copy(src, &tmp)
        .map_err(|e| format!("Could not copy to {}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
    }
    let _ = std::fs::remove_file(&p.new);
    std::fs::rename(&tmp, &p.new).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("Could not create {}: {e}", p.new.display())
    })?;
    Ok(n)
}

/// Remove a staged update.
pub fn discard(p: &Paths) -> Result<(), String> {
    match std::fs::remove_file(&p.new) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Could not delete {}: {e}", p.new.display())),
    }
}

/// What [`install`] did, step by step.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub steps: Vec<String>,
    /// The new process's id when it started.
    pub pid: Option<u32>,
    pub error: Option<String>,
    /// Everything was put back after a failure.
    pub rolled_back: bool,
}

/// Swap the files and start the new version with `launch`. On any failure
/// the files are put back (`Outcome::rolled_back`).
pub fn install(p: &Paths, launch: impl FnOnce(&Path) -> std::io::Result<u32>) -> Outcome {
    let mut o = Outcome::default();
    if !p.new.is_file() {
        o.error = Some(format!("No staged update at {}", p.new.display()));
        return o;
    }
    if p.old.exists() {
        if let Err(e) = std::fs::remove_file(&p.old) {
            o.error = Some(format!(
                "Could not delete {} ({e}); is an older Hadion still running?",
                p.old.display()
            ));
            return o;
        }
        o.steps.push(format!("deleted {}", file_name(&p.old)));
    }
    if let Err(e) = std::fs::rename(&p.exe, &p.old) {
        o.error = Some(format!(
            "Could not rename {} to {}: {e}",
            p.exe.display(),
            file_name(&p.old)
        ));
        return o;
    }
    o.steps.push(format!(
        "renamed {} → {}",
        file_name(&p.exe),
        file_name(&p.old)
    ));
    if let Err(e) = std::fs::rename(&p.new, &p.exe) {
        o.error = Some(format!(
            "Could not move {} into place: {e}",
            file_name(&p.new)
        ));
        o.rolled_back = restore(p, false, &mut o.steps);
        return o;
    }
    o.steps.push(format!(
        "renamed {} → {}",
        file_name(&p.new),
        file_name(&p.exe)
    ));
    match launch(&p.exe) {
        Ok(pid) => {
            o.steps
                .push(format!("started {} (pid {pid})", p.exe.display()));
            o.pid = Some(pid);
        }
        Err(e) => {
            o.error = Some(format!("Could not start the new version: {e}"));
            o.rolled_back = restore(p, true, &mut o.steps);
        }
    }
    o
}

/// Undo the swap: the new file goes back to `.new` (when it was moved into
/// place), `.old` back to the executable's name.
fn restore(p: &Paths, new_in_place: bool, steps: &mut Vec<String>) -> bool {
    if new_in_place {
        match std::fs::rename(&p.exe, &p.new) {
            Ok(()) => steps.push(format!(
                "rolled back: renamed {} → {}",
                file_name(&p.exe),
                file_name(&p.new)
            )),
            Err(e) => {
                steps.push(format!("rollback failed: {e}"));
                return false;
            }
        }
    }
    match std::fs::rename(&p.old, &p.exe) {
        Ok(()) => {
            steps.push(format!(
                "rolled back: renamed {} → {}",
                file_name(&p.old),
                file_name(&p.exe)
            ));
            true
        }
        Err(e) => {
            steps.push(format!("rollback failed: {e}"));
            false
        }
    }
}

/// Delete `<exe>.old`, retrying while the previous process is still
/// exiting (Windows refuses to delete a running image). Returns whether it
/// is gone.
pub fn cleanup_old(p: &Paths, attempts: u32, wait: std::time::Duration) -> bool {
    for i in 0..attempts.max(1) {
        match std::fs::remove_file(&p.old) {
            Ok(()) => return true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return true,
            Err(_) if i + 1 < attempts => std::thread::sleep(wait),
            Err(_) => {}
        }
    }
    !p.old.exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn sandbox(name: &str) -> (PathBuf, Paths) {
        let dir = std::env::temp_dir().join(format!("hadion-swap-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("hadion.exe");
        fs::write(&exe, b"old version").unwrap();
        (dir, Paths::for_exe(&exe))
    }

    fn read(p: &Path) -> String {
        fs::read_to_string(p).unwrap_or_else(|_| "<missing>".into())
    }

    #[test]
    fn stage_install_and_cleanup() {
        let (dir, p) = sandbox("ok");
        assert_eq!(p.new.file_name().unwrap(), "hadion.exe.new");
        assert_eq!(p.old.file_name().unwrap(), "hadion.exe.old");
        let src = dir.join("download.exe");
        fs::write(&src, b"new version").unwrap();
        assert!(!p.staged());
        assert_eq!(stage(&src, &p).unwrap(), 11);
        assert!(p.staged());
        assert!(!dir.join("hadion.exe.new.part").exists());
        // A leftover from an earlier update is removed first.
        fs::write(&p.old, b"ancient").unwrap();
        let plan = p.plan(&["C:/repos/a b".to_owned()]);
        assert!(plan[0].starts_with("delete "), "{plan:?}");
        assert!(
            plan.iter().any(|l| l.ends_with("\"C:/repos/a b\"")),
            "{plan:?}"
        );
        let mut launched = None;
        let o = install(&p, |exe| {
            launched = Some(read(exe));
            Ok(4242)
        });
        assert_eq!(o.error, None, "{o:?}");
        assert_eq!(o.pid, Some(4242));
        assert_eq!(launched.as_deref(), Some("new version"));
        assert_eq!(read(&p.exe), "new version");
        assert_eq!(read(&p.old), "old version");
        assert!(!p.new.exists());
        assert_eq!(o.steps.len(), 4, "{:?}", o.steps);
        assert!(cleanup_old(&p, 3, std::time::Duration::from_millis(1)));
        assert!(!p.old.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_launch_rolls_back() {
        let (dir, p) = sandbox("rollback");
        fs::write(&p.new, b"new version").unwrap();
        let o = install(&p, |_| {
            Err(std::io::Error::other("not a valid application"))
        });
        assert!(o.error.unwrap().contains("not a valid application"));
        assert!(o.rolled_back, "{:?}", o.steps);
        assert_eq!(read(&p.exe), "old version");
        assert_eq!(read(&p.new), "new version");
        assert!(!p.old.exists());
        assert!(o.steps.iter().any(|s| s.starts_with("rolled back")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_staged_changes_nothing() {
        let (dir, p) = sandbox("none");
        let o = install(&p, |_| panic!("must not launch"));
        assert!(o.error.unwrap().contains("No staged update"));
        assert_eq!(read(&p.exe), "old version");
        assert!(discard(&p).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_exe_fails_before_anything_moves() {
        let (dir, p) = sandbox("noexe");
        fs::remove_file(&p.exe).unwrap();
        fs::write(&p.new, b"new version").unwrap();
        let o = install(&p, |_| Ok(1));
        assert!(o.error.unwrap().contains("Could not rename"));
        assert_eq!(read(&p.new), "new version");
        assert!(!p.old.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cargo_target_folders_are_refused() {
        let dir = std::env::temp_dir().join(format!("hadion-swap-cargo-{}", std::process::id()));
        let debug = dir.join("target").join("debug");
        fs::create_dir_all(debug.join(".fingerprint")).unwrap();
        assert!(dev_build_reason(&debug.join("hadion.exe")).is_some());
        let release = dir.join("target").join("release");
        fs::create_dir_all(&release).unwrap();
        fs::write(dir.join("target").join("CACHEDIR.TAG"), b"").unwrap();
        assert!(dev_build_reason(&release.join("hadion.exe")).is_some());
        let installed = dir.join("Programs").join("hadion");
        fs::create_dir_all(&installed).unwrap();
        assert_eq!(dev_build_reason(&installed.join("hadion.exe")), None);
        assert!(writable_dir(&installed.join("hadion.exe")).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }
}
