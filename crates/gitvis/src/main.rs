#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod format;
mod highlight;
mod script;
mod state;
mod theme;
mod views;
mod watcher;
mod worker;

fn main() -> eframe::Result<()> {
    // `gitvis [path...]` opens those repositories (defaults to the current
    // directory when it is inside a repository).
    let mut open: Vec<std::path::PathBuf> = std::env::args_os().skip(1).map(Into::into).collect();
    if open.is_empty()
        && let Ok(cwd) = std::env::current_dir()
        && gitvis_core::repo::locate(&cwd).is_ok()
    {
        open.push(cwd);
    }
    // Scripted smoke tests must not touch the user's saved session (open
    // tabs, window geometry, per-repository preferences).
    let scripted = std::env::var_os("GITVIS_SCRIPT").is_some();
    let options = eframe::NativeOptions {
        persist_window: !scripted,
        viewport: egui::ViewportBuilder::default()
            .with_title("gitvis")
            .with_app_id("gitvis")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([900.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "gitvis",
        options,
        Box::new(move |cc| Ok(Box::new(app::GitVis::new(cc, open)))),
    )
}
