//! What this build is and where its files live: the values `build.rs`
//! embedded, the OS version (probed once in the background, read by the
//! panic hook), the executable, settings, data and crash folders.

use std::path::PathBuf;
use std::sync::OnceLock;

use gitvis_core::devops::packaging::buildinfo::{BuildInfo, Embedded};
use gitvis_core::devops::packaging::paths;

/// This build, from `build.rs` (every value has a fallback).
pub fn build() -> &'static BuildInfo {
    static INFO: OnceLock<BuildInfo> = OnceLock::new();
    INFO.get_or_init(|| {
        BuildInfo::from_embedded(Embedded {
            version: env!("CARGO_PKG_VERSION"),
            commit: option_env!("GITVIS_BUILD_COMMIT"),
            dirty: option_env!("GITVIS_BUILD_DIRTY"),
            date: option_env!("GITVIS_BUILD_DATE"),
            target: option_env!("GITVIS_BUILD_TARGET"),
            profile: option_env!("GITVIS_BUILD_PROFILE"),
            rustc: option_env!("GITVIS_BUILD_RUSTC"),
            remote: option_env!("GITVIS_BUILD_REMOTE"),
        })
    })
}

static OS: OnceLock<String> = OnceLock::new();

/// The OS name and version once [`probe_os`] has finished, else the bare
/// OS and architecture.
pub fn os() -> String {
    OS.get()
        .cloned()
        .unwrap_or_else(|| format!("{}, {}", std::env::consts::OS, std::env::consts::ARCH))
}

/// Read the OS version in a background thread (`cmd /c ver` on Windows,
/// `sw_vers` on macOS, `uname -sr` elsewhere), so a crash report can name
/// it without starting a process from the panic hook.
pub fn probe_os() {
    if OS.get().is_some() {
        return;
    }
    std::thread::Builder::new()
        .name("os-version".into())
        .spawn(|| {
            let v = os_version().unwrap_or_default();
            let arch = std::env::consts::ARCH;
            let _ = OS.set(if v.is_empty() {
                format!("{}, {arch}", std::env::consts::OS)
            } else {
                format!("{v}, {arch}")
            });
        })
        .ok();
}

fn run(program: &str, args: &[&str]) -> Option<String> {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn os_version() -> Option<String> {
    match std::env::consts::OS {
        // "Microsoft Windows [Version 10.0.26300.6899]"
        "windows" => {
            let text = run("cmd", &["/c", "ver"])?;
            let v = text
                .split("Version")
                .nth(1)?
                .trim_matches(|c: char| c == ']' || c.is_whitespace())
                .to_owned();
            let build: u32 = v.split('.').nth(2)?.parse().ok()?;
            let name = if v.starts_with("10.0") && build >= 22_000 {
                "Windows 11"
            } else if v.starts_with("10.0") {
                "Windows 10"
            } else {
                "Windows"
            };
            Some(format!("{name} ({v})"))
        }
        "macos" => run("sw_vers", &["-productVersion"]).map(|v| format!("macOS {v}")),
        _ => run("uname", &["-sr"]),
    }
}

/// The running executable.
pub fn exe() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

/// eframe's settings file for the app id `gitvis`.
pub fn settings_file() -> Option<PathBuf> {
    eframe::storage_dir("gitvis").map(|d| d.join("app.ron"))
}

pub fn data_dir() -> Option<PathBuf> {
    paths::data_dir()
}

pub fn crash_dir() -> Option<PathBuf> {
    paths::crash_dir()
}

pub fn update_log() -> Option<PathBuf> {
    paths::update_log()
}

/// The user's home folder (masked as `~` in issue text).
pub fn home() -> Option<String> {
    std::env::var("USERPROFILE")
        .ok()
        .or_else(|| std::env::var("HOME").ok())
        .filter(|h| !h.trim().is_empty())
}
