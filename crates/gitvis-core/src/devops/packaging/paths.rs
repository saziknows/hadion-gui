//! Where gitvis keeps machine-local files (not settings: eframe stores
//! those under `%APPDATA%\gitvis\data` and equivalents).
//!
//! | What | Default | Override |
//! |---|---|---|
//! | Data folder | `%LOCALAPPDATA%\gitvis`, `~/Library/Application Support/gitvis`, `$XDG_DATA_HOME/gitvis` or `~/.local/share/gitvis` | `GITVIS_DATA_DIR` |
//! | Crash reports | `<data>/crashes` | `GITVIS_CRASH_DIR` |
//! | Update log | `<data>/update.log` | (follows the data folder) |
//! | Downloads | `<temp>/gitvis-update/<tag>` | (follows `TEMP`) |
//! | Test releases | none | `GITVIS_UPDATE_DIR` (a folder with `releases.json` and the assets) |

use std::path::PathBuf;

fn nonempty(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

/// The data folder for `os` given environment lookups (pure, for tests).
pub fn data_dir_with(env: impl Fn(&str) -> Option<String>, os: &str) -> Option<PathBuf> {
    if let Some(d) = nonempty(env("GITVIS_DATA_DIR")) {
        return Some(PathBuf::from(d));
    }
    let home = || nonempty(env("HOME")).or_else(|| nonempty(env("USERPROFILE")));
    match os {
        "windows" => nonempty(env("LOCALAPPDATA"))
            .map(PathBuf::from)
            .or_else(|| {
                nonempty(env("USERPROFILE")).map(|h| PathBuf::from(h).join("AppData").join("Local"))
            })
            .map(|p| p.join("gitvis")),
        "macos" => home().map(|h| {
            PathBuf::from(h)
                .join("Library")
                .join("Application Support")
                .join("gitvis")
        }),
        _ => nonempty(env("XDG_DATA_HOME"))
            .filter(|d| d.starts_with('/'))
            .map(PathBuf::from)
            .or_else(|| home().map(|h| PathBuf::from(h).join(".local").join("share")))
            .map(|p| p.join("gitvis")),
    }
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

/// The data folder on this machine.
pub fn data_dir() -> Option<PathBuf> {
    data_dir_with(env, std::env::consts::OS)
}

/// The crash report folder for given lookups (pure, for tests).
pub fn crash_dir_with(env: impl Fn(&str) -> Option<String>, os: &str) -> Option<PathBuf> {
    if let Some(d) = nonempty(env("GITVIS_CRASH_DIR")) {
        return Some(PathBuf::from(d));
    }
    data_dir_with(env, os).map(|d| d.join("crashes"))
}

pub fn crash_dir() -> Option<PathBuf> {
    crash_dir_with(env, std::env::consts::OS)
}

pub fn update_log() -> Option<PathBuf> {
    data_dir().map(|d| d.join("update.log"))
}

/// The test-mode release folder (`GITVIS_UPDATE_DIR`).
pub fn test_dir() -> Option<PathBuf> {
    nonempty(env("GITVIS_UPDATE_DIR")).map(PathBuf::from)
}

/// Where a release's files are downloaded to.
pub fn download_dir(tag: &str) -> PathBuf {
    let safe: String = tag
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    std::env::temp_dir().join("gitvis-update").join(safe)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn per_os_defaults_and_overrides() {
        let win = lookup(&[("LOCALAPPDATA", "C:\\Users\\u\\AppData\\Local")]);
        assert_eq!(
            data_dir_with(&win, "windows").unwrap(),
            PathBuf::from("C:\\Users\\u\\AppData\\Local").join("gitvis")
        );
        assert_eq!(
            crash_dir_with(&win, "windows").unwrap(),
            PathBuf::from("C:\\Users\\u\\AppData\\Local")
                .join("gitvis")
                .join("crashes")
        );
        let mac = lookup(&[("HOME", "/Users/u")]);
        assert_eq!(
            data_dir_with(&mac, "macos").unwrap(),
            PathBuf::from("/Users/u/Library/Application Support/gitvis")
        );
        let linux = lookup(&[("HOME", "/home/u"), ("XDG_DATA_HOME", "relative/ignored")]);
        assert_eq!(
            data_dir_with(&linux, "linux").unwrap(),
            PathBuf::from("/home/u/.local/share/gitvis")
        );
        let xdg = lookup(&[("HOME", "/home/u"), ("XDG_DATA_HOME", "/data")]);
        assert_eq!(
            data_dir_with(&xdg, "linux").unwrap(),
            PathBuf::from("/data/gitvis")
        );
        let o = lookup(&[
            ("LOCALAPPDATA", "C:\\x"),
            ("GITVIS_DATA_DIR", "D:\\scratch"),
            ("GITVIS_CRASH_DIR", " "),
        ]);
        assert_eq!(
            data_dir_with(&o, "windows").unwrap(),
            PathBuf::from("D:\\scratch")
        );
        assert_eq!(
            crash_dir_with(&o, "windows").unwrap(),
            PathBuf::from("D:\\scratch").join("crashes")
        );
        let c = lookup(&[("GITVIS_CRASH_DIR", "/tmp/c")]);
        assert_eq!(
            crash_dir_with(&c, "linux").unwrap(),
            PathBuf::from("/tmp/c")
        );
        assert_eq!(data_dir_with(lookup(&[]), "linux"), None);
    }

    #[test]
    fn download_folder_names_are_safe() {
        let d = download_dir("v0.2.0/../../x");
        assert!(d.ends_with("v0.2.0_.._.._x"), "{}", d.display());
    }
}
