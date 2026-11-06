//! Sweeper Kit desktop app: Tauri shell around `sweeper-core`.

pub mod commands;
pub mod state;

use std::path::PathBuf;

use tauri::{DragDropEvent, Emitter, Manager, WindowEvent};

use commands::wrappers as cmd;
use state::{lock, AppContext, AppState};

/// Event the UI listens to for an app dropped on the window or Dock icon.
const OPEN_APP_EVENT: &str = "open-app";

/// Keeps only `.app` bundle paths.
fn app_bundles(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    paths
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")))
        .collect()
}

/// Hands the first app bundle in `paths` to the UI, and remembers it in case
/// the UI has not started listening yet.
fn open_app(handle: &tauri::AppHandle, paths: Vec<PathBuf>) {
    let Some(path) = app_bundles(paths).into_iter().next() else {
        return;
    };
    if let Some(state) = handle.try_state::<AppState>() {
        *lock(&state.pending_open) = Some(path.clone());
    }
    let _ = handle.emit(OPEN_APP_EVENT, path);
}

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::new(AppContext::production()))
        .on_window_event(|window, event| {
            if let WindowEvent::DragDrop(DragDropEvent::Drop { paths, .. }) = event {
                open_app(window.app_handle(), paths.clone());
            }
        })
        .invoke_handler(tauri::generate_handler![
            cmd::list_apps,
            cmd::scan_app,
            cmd::apply_preset,
            cmd::plan_removal,
            cmd::running_instances,
            cmd::quit_apps,
            cmd::uninstall,
            cmd::brew_zap_command,
            cmd::brew_zap,
            cmd::fda_status,
            cmd::open_fda_settings,
            cmd::reveal_in_finder,
            cmd::get_settings,
            cmd::save_settings,
            cmd::undo_entries,
            cmd::rules_warning,
            cmd::take_pending_open,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build the Sweeper Kit application");

    app.run(|handle, event| {
        // An .app dropped on the Dock icon or opened with Sweeper Kit.
        #[cfg(target_os = "macos")]
        if let tauri::RunEvent::Opened { urls } = &event {
            let paths = urls.iter().filter_map(|u| u.to_file_path().ok()).collect();
            open_app(handle, paths);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (handle, event);
    });
}
