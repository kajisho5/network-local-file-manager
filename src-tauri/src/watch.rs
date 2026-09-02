//! Wires a watched folder's local change events into peer broadcast + a toast.
//!
//! Also runs startup reconciliation ([`lfsync_core::ManifestStore`]) so changes made
//! while this folder wasn't being watched (app closed, PC asleep, ...) are still
//! reported once watching resumes — see that module's docs for what this does and
//! doesn't cover.

use crate::notify_toast::show_change_toast;
use crate::state::AppState;
use lfsync_core::{
    broadcast, ChangeEvent, ChangeMessage, ExcludeRules, FolderWatcher, ManifestStore, SharedSecret,
};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
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
    let excludes = ExcludeRules::default();
    let manifest_path = crate::config::manifests_dir().join(manifest_filename(folder));
    let (store, catchup_events) = ManifestStore::open(root.clone(), manifest_path, &excludes);
    if !catchup_events.is_empty() {
        tracing::info!(
            folder,
            count = catchup_events.len(),
            "found changes made while this folder wasn't being watched"
        );
    }

    let (tx, rx) = mpsc::unbounded_channel::<ChangeEvent>();
    let watcher =
        FolderWatcher::watch(&root, DEBOUNCE, excludes, tx).map_err(|err| err.to_string())?;
    app.state::<AppState>()
        .watchers
        .lock()
        .unwrap()
        .insert(folder.to_string(), watcher);

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        forward_changes(app, root, store, catchup_events, rx).await;
    });

    Ok(())
}

/// A short, filesystem-safe identifier for a watched folder's manifest file. Doesn't
/// need to be cryptographically strong — just stable and unique enough to avoid
/// collisions across the handful of folders a person is likely to watch.
fn manifest_filename(folder: &str) -> String {
    let mut hasher = DefaultHasher::new();
    folder.hash(&mut hasher);
    format!("{:016x}.json", hasher.finish())
}

async fn forward_changes(
    app: AppHandle,
    root: PathBuf,
    store: ManifestStore,
    catchup_events: Vec<ChangeEvent>,
    mut rx: mpsc::UnboundedReceiver<ChangeEvent>,
) {
    // Catch-up events already reflect the manifest `store` just persisted at open time,
    // so unlike live events below they don't need a `store.record` call.
    for event in catchup_events {
        report_change(&app, &root, &event).await;
    }

    while let Some(event) = rx.recv().await {
        report_change(&app, &root, &event).await;
        store.record(&event);
    }
}

async fn report_change(app: &AppHandle, root: &Path, event: &ChangeEvent) {
    let (peer_id, hostname, shared_secret, peer_registry) = {
        let state = app.state::<AppState>();
        let config = state.config.lock().unwrap();
        (
            config.peer_id.clone(),
            state.hostname.clone(),
            SharedSecret::new(&config.shared_secret),
            state.peer_registry.clone(),
        )
    };

    let display_path = relative_display_path(root, &event.path);
    let msg = ChangeMessage {
        peer_id,
        hostname,
        path: display_path.clone(),
        kind: event.kind,
        timestamp: chrono::Utc::now(),
    };

    broadcast(&peer_registry, &shared_secret, &msg).await;
    show_change_toast(app, "このPC", &display_path, event.kind);
}

fn relative_display_path(root: &Path, full_path: &Path) -> String {
    full_path
        .strip_prefix(root)
        .unwrap_or(full_path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_root_and_normalizes_separators() {
        let root = Path::new("/home/alice/Documents");
        let full = Path::new("/home/alice/Documents/reports/q1.docx");
        assert_eq!(relative_display_path(root, full), "reports/q1.docx");
    }

    #[test]
    fn falls_back_to_full_path_when_not_under_root() {
        let root = Path::new("/home/alice/Documents");
        let unrelated = Path::new("/tmp/scratch.txt");
        assert_eq!(
            relative_display_path(root, unrelated),
            unrelated.to_string_lossy()
        );
    }

    #[test]
    fn manifest_filename_is_stable_and_distinguishes_folders() {
        let a1 = manifest_filename("/home/alice/Documents");
        let a2 = manifest_filename("/home/alice/Documents");
        let b = manifest_filename("/home/alice/Photos");
        assert_eq!(a1, a2);
        assert_ne!(a1, b);
        assert!(a1.ends_with(".json"));
    }
}
