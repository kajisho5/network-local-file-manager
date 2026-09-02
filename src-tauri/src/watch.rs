//! Wires a watched folder's local change events into peer broadcast + a toast.

use crate::notify_toast::show_change_toast;
use crate::state::AppState;
use lfsync_core::{broadcast, ChangeEvent, ChangeMessage, FolderWatcher};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc;

/// How long a path must be quiet before its change is reported. Coalesces the burst of
/// events a single save typically produces (e.g. create + several writes).
const DEBOUNCE: Duration = Duration::from_millis(800);

/// Starts watching `folder`, registering the watcher in [`AppState::watchers`] and
/// spawning a task that broadcasts each detected change to known peers and shows a
/// local toast confirming it was picked up.
pub fn start_watching_folder(app: &AppHandle, folder: &str) -> Result<(), String> {
    let root = PathBuf::from(folder);
    let (tx, rx) = mpsc::unbounded_channel::<ChangeEvent>();

    let watcher = FolderWatcher::watch(&root, DEBOUNCE, tx).map_err(|err| err.to_string())?;
    app.state::<AppState>()
        .watchers
        .lock()
        .unwrap()
        .insert(folder.to_string(), watcher);

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        forward_local_changes(app, root, rx).await;
    });

    Ok(())
}

async fn forward_local_changes(
    app: AppHandle,
    root: PathBuf,
    mut rx: mpsc::UnboundedReceiver<ChangeEvent>,
) {
    while let Some(event) = rx.recv().await {
        let (peer_id, hostname, peer_registry) = {
            let state = app.state::<AppState>();
            let peer_id = state.config.lock().unwrap().peer_id.clone();
            (peer_id, state.hostname.clone(), state.peer_registry.clone())
        };

        let display_path = relative_display_path(&root, &event.path);
        let msg = ChangeMessage {
            peer_id,
            hostname,
            path: display_path.clone(),
            kind: event.kind,
            timestamp: chrono::Utc::now(),
        };

        broadcast(&peer_registry, &msg).await;
        show_change_toast(&app, "このPC", &display_path, event.kind);
    }
}

fn relative_display_path(root: &Path, full_path: &Path) -> String {
    full_path
        .strip_prefix(root)
        .unwrap_or(full_path)
        .to_string_lossy()
        .replace('\\', "/")
}
