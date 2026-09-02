//! Tauri commands invoked from the settings window's JavaScript frontend.

use crate::state::AppState;
use crate::watch::start_watching_folder;
use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

#[tauri::command]
pub fn get_watched_folders(state: State<AppState>) -> Vec<String> {
    state.config.lock().unwrap().watched_folders.clone()
}

/// Opens the native "choose a folder" dialog and returns the chosen path, or `None` if
/// the user cancelled. Done from the Rust side so the frontend only needs `invoke`.
#[tauri::command]
pub fn pick_folder(app: AppHandle) -> Option<String> {
    app.dialog()
        .file()
        .blocking_pick_folder()
        .map(|path| path.to_string())
}

#[derive(Serialize)]
pub struct PeerInfo {
    hostname: String,
    addr: String,
}

#[tauri::command]
pub fn get_peers(state: State<AppState>) -> Vec<PeerInfo> {
    state
        .peer_registry
        .handles()
        .into_iter()
        .map(|h| PeerInfo {
            hostname: h.hostname,
            addr: h.addr.to_string(),
        })
        .collect()
}

#[tauri::command]
pub fn add_watched_folder(
    app: AppHandle,
    state: State<AppState>,
    path: String,
) -> Result<(), String> {
    {
        let mut config = state.config.lock().unwrap();
        if config.watched_folders.iter().any(|p| p == &path) {
            return Ok(());
        }
        config.watched_folders.push(path.clone());
        config
            .save(&state.config_path)
            .map_err(|err| err.to_string())?;
    }
    start_watching_folder(&app, &path)
}

#[tauri::command]
pub fn remove_watched_folder(state: State<AppState>, path: String) -> Result<(), String> {
    {
        let mut config = state.config.lock().unwrap();
        config.watched_folders.retain(|p| p != &path);
        config
            .save(&state.config_path)
            .map_err(|err| err.to_string())?;
    }
    // Dropping the watcher stops it.
    state.watchers.lock().unwrap().remove(&path);
    Ok(())
}
