//! Where Hadion keeps machine-local files (not settings: eframe stores
//! those under `%APPDATA%\hadion\data` and equivalents).
//!
//! | What | Default | Override |
//! |---|---|---|
//! | Data folder | `%LOCALAPPDATA%\hadion`, `~/Library/Application Support/hadion`, `$XDG_DATA_HOME/hadion` or `~/.local/share/hadion` | `HADION_DATA_DIR` |
//! | Crash reports | `<data>/crashes` | `HADION_CRASH_DIR` |
//! | Update log | `<data>/update.log` | (follows the data folder) |
//! | Downloads | `<temp>/hadion-update/<tag>` | (follows `TEMP`) |
//! | Test releases | none | `HADION_UPDATE_DIR` (a folder with `releases.json` and the assets) |

use std::path::PathBuf;

fn nonempty(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

/// The data folder for `os` given environment lookups (pure, for tests).
pub fn data_dir_with(env: impl Fn(&str) -> Option<String>, os: &str) -> Option<PathBuf> {
    if let Some(d) = nonempty(env("HADION_DATA_DIR")) {
        return Some(PathBuf::from(d));
    }
    data_dir_named(env, os, "hadion")
}

/// Where the data folder was before the rename from gitvis; `None` with an
/// override, which has nothing to carry over.
pub fn legacy_data_dir_with(env: impl Fn(&str) -> Option<String>, os: &str) -> Option<PathBuf> {
    if nonempty(env("HADION_DATA_DIR")).is_some() {
        return None;
    }
    data_dir_named(env, os, "gitvis")
}

pub fn legacy_data_dir() -> Option<PathBuf> {
    legacy_data_dir_with(env, std::env::consts::OS)
}

fn data_dir_named(env: impl Fn(&str) -> Option<String>, os: &str, name: &str) -> Option<PathBuf> {
    let home = || nonempty(env("HOME")).or_else(|| nonempty(env("USERPROFILE")));
    match os {
        "windows" => nonempty(env("LOCALAPPDATA"))
            .map(PathBuf::from)
            .or_else(|| {
                nonempty(env("USERPROFILE")).map(|h| PathBuf::from(h).join("AppData").join("Local"))
            })
            .map(|p| p.join(name)),
        "macos" => home().map(|h| {
            PathBuf::from(h)
                .join("Library")
                .join("Application Support")
                .join(name)
        }),
        _ => nonempty(env("XDG_DATA_HOME"))
            .filter(|d| d.starts_with('/'))
            .map(PathBuf::from)
            .or_else(|| home().map(|h| PathBuf::from(h).join(".local").join("share")))
            .map(|p| p.join(name)),
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
    if let Some(d) = nonempty(env("HADION_CRASH_DIR")) {
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

/// The test-mode release folder (`HADION_UPDATE_DIR`).
pub fn test_dir() -> Option<PathBuf> {
    nonempty(env("HADION_UPDATE_DIR")).map(PathBuf::from)
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
    std::env::temp_dir().join("hadion-update").join(safe)
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
    fn legacy_folder_is_the_gitvis_one_unless_overridden() {
        let win = lookup(&[("LOCALAPPDATA", "C:\\Users\\u\\AppData\\Local")]);
        assert_eq!(
            legacy_data_dir_with(&win, "windows").unwrap(),
            PathBuf::from("C:\\Users\\u\\AppData\\Local").join("gitvis")
        );
        let linux = lookup(&[("HOME", "/home/u")]);
        assert_eq!(
            legacy_data_dir_with(&linux, "linux").unwrap(),
            PathBuf::from("/home/u/.local/share/gitvis")
        );
        let o = lookup(&[("LOCALAPPDATA", "C:\\x"), ("HADION_DATA_DIR", "D:\\s")]);
        assert_eq!(legacy_data_dir_with(&o, "windows"), None);
    }

    #[test]
    fn per_os_defaults_and_overrides() {
        let win = lookup(&[("LOCALAPPDATA", "C:\\Users\\u\\AppData\\Local")]);
        assert_eq!(
            data_dir_with(&win, "windows").unwrap(),
            PathBuf::from("C:\\Users\\u\\AppData\\Local").join("hadion")
        );
        assert_eq!(
            crash_dir_with(&win, "windows").unwrap(),
            PathBuf::from("C:\\Users\\u\\AppData\\Local")
                .join("hadion")
                .join("crashes")
        );
        let mac = lookup(&[("HOME", "/Users/u")]);
        assert_eq!(
            data_dir_with(&mac, "macos").unwrap(),
            PathBuf::from("/Users/u/Library/Application Support/hadion")
        );
        let linux = lookup(&[("HOME", "/home/u"), ("XDG_DATA_HOME", "relative/ignored")]);
        assert_eq!(
            data_dir_with(&linux, "linux").unwrap(),
            PathBuf::from("/home/u/.local/share/hadion")
        );
        let xdg = lookup(&[("HOME", "/home/u"), ("XDG_DATA_HOME", "/data")]);
        assert_eq!(
            data_dir_with(&xdg, "linux").unwrap(),
            PathBuf::from("/data/hadion")
        );
        let o = lookup(&[
            ("LOCALAPPDATA", "C:\\x"),
            ("HADION_DATA_DIR", "D:\\scratch"),
            ("HADION_CRASH_DIR", " "),
        ]);
        assert_eq!(
            data_dir_with(&o, "windows").unwrap(),
            PathBuf::from("D:\\scratch")
        );
        assert_eq!(
            crash_dir_with(&o, "windows").unwrap(),
            PathBuf::from("D:\\scratch").join("crashes")
        );
        let c = lookup(&[("HADION_CRASH_DIR", "/tmp/c")]);
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
