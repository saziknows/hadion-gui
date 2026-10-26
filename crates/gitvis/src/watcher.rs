//! File-system watching for automatic refresh.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::state::RepoId;
use crate::worker::Event;

pub struct Watcher {
    _inner: RecommendedWatcher,
}

pub fn watch(
    repo: RepoId,
    workdir: &Path,
    git_dir: &Path,
    tx: Sender<Event>,
    ctx: egui::Context,
) -> Result<Watcher, String> {
    let git_dir_owned: PathBuf = git_dir.to_path_buf();
    let mut inner = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        let mut full = false;
        let mut any = false;
        for p in &event.paths {
            match classify(p, &git_dir_owned) {
                Interest::Ignore => {}
                Interest::Status => any = true,
                Interest::Full => {
                    any = true;
                    full = true;
                }
            }
        }
        if any {
            let _ = tx.send(Event::Watch { repo, full });
            ctx.request_repaint();
        }
    })
    .map_err(|e| e.to_string())?;
    inner
        .watch(workdir, RecursiveMode::Recursive)
        .map_err(|e| e.to_string())?;
    if !git_dir.starts_with(workdir) {
        inner
            .watch(git_dir, RecursiveMode::Recursive)
            .map_err(|e| e.to_string())?;
    }
    Ok(Watcher { _inner: inner })
}

enum Interest {
    Ignore,
    Status,
    Full,
}

fn classify(path: &Path, git_dir: &Path) -> Interest {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.ends_with(".lock") {
        return Interest::Ignore;
    }
    let Ok(rel) = path.strip_prefix(git_dir) else {
        // A work tree file.
        return Interest::Status;
    };
    let first = rel
        .components()
        .next()
        .and_then(|c| c.as_os_str().to_str())
        .unwrap_or("");
    match first {
        "objects" | "lfs" | "hooks" | "info" | "gitvis" => Interest::Ignore,
        "index" => Interest::Status,
        "" => Interest::Ignore,
        _ => Interest::Full,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        let g = Path::new("/r/.git");
        assert!(matches!(
            classify(Path::new("/r/src/a.rs"), g),
            Interest::Status
        ));
        assert!(matches!(
            classify(Path::new("/r/.git/index"), g),
            Interest::Status
        ));
        assert!(matches!(
            classify(Path::new("/r/.git/index.lock"), g),
            Interest::Ignore
        ));
        assert!(matches!(
            classify(Path::new("/r/.git/objects/ab/cd"), g),
            Interest::Ignore
        ));
        assert!(matches!(
            classify(Path::new("/r/.git/refs/heads/main"), g),
            Interest::Full
        ));
        assert!(matches!(
            classify(Path::new("/r/.git/HEAD"), g),
            Interest::Full
        ));
        assert!(matches!(
            classify(Path::new("/r/.git/MERGE_HEAD"), g),
            Interest::Full
        ));
    }
}
