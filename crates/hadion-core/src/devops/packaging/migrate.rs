//! Carrying folders over from gitvis, the name before Hadion: settings
//! (eframe's storage folder) and the data folder are copied once, when the
//! new folder does not exist yet. The old folders stay, so an older build
//! still finds its own.

use std::io;
use std::path::Path;

/// Copy `old` to `new` when `new` does not exist (or is an empty folder)
/// and `old` is a folder. The copy is made beside `new` and renamed into
/// place, so an interrupted copy is never mistaken for a finished one.
/// Returns whether it copied.
pub fn carry_over(old: &Path, new: &Path) -> io::Result<bool> {
    if !old.is_dir() {
        return Ok(false);
    }
    // eframe creates its storage folder when it starts, scripted runs
    // included, so an empty one holds nothing to keep. `remove_dir` refuses
    // a folder that is no longer empty.
    if std::fs::read_dir(new).is_ok_and(|mut d| d.next().is_none()) {
        let _ = std::fs::remove_dir(new);
    }
    if new.exists() {
        return Ok(false);
    }
    let parent = new
        .parent()
        .ok_or_else(|| io::Error::other("the new folder has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let name = new.file_name().and_then(|n| n.to_str()).unwrap_or("data");
    let staging = parent.join(format!(".{name}.carry-over-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let copied = copy_tree(old, &staging).and_then(|()| std::fs::rename(&staging, new));
    if copied.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    // Another process may have created `new` meanwhile: that one wins.
    match copied {
        Err(_) if new.exists() => Ok(false),
        Err(e) => Err(e),
        Ok(()) => Ok(true),
    }
}

/// Files and folders under `from`, copied to `to` (links are skipped).
fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hadion-carry-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn copies_once_and_keeps_the_old_folder() {
        let d = temp("once");
        let old = d.join("gitvis");
        std::fs::create_dir_all(old.join("data")).unwrap();
        std::fs::write(old.join("data").join("app.ron"), "(settings)").unwrap();
        let new = d.join("hadion");
        assert!(carry_over(&old, &new).unwrap());
        assert_eq!(
            std::fs::read_to_string(new.join("data").join("app.ron")).unwrap(),
            "(settings)"
        );
        assert!(old.join("data").join("app.ron").exists());
        // Nothing staged is left behind.
        let left: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left.len(), 2, "{left:?}");

        // A second run (or a newer settings file) never overwrites.
        std::fs::write(new.join("data").join("app.ron"), "(newer)").unwrap();
        assert!(!carry_over(&old, &new).unwrap());
        assert_eq!(
            std::fs::read_to_string(new.join("data").join("app.ron")).unwrap(),
            "(newer)"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_empty_new_folder_is_replaced() {
        let d = temp("empty");
        let old = d.join("gitvis");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("app.ron"), "(settings)").unwrap();
        let new = d.join("hadion");
        std::fs::create_dir_all(&new).unwrap();
        assert!(carry_over(&old, &new).unwrap());
        assert_eq!(
            std::fs::read_to_string(new.join("app.ron")).unwrap(),
            "(settings)"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn nothing_to_carry_over() {
        let d = temp("none");
        assert!(!carry_over(&d.join("missing"), &d.join("hadion")).unwrap());
        assert!(!d.join("hadion").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
