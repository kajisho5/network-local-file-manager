//! Shared application state accessible from Tauri commands.

use crate::config::Config;
use lfsync_core::{FolderWatcher, Outbox, PeerRegistry, Roster};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

pub struct AppState {
    pub config_path: PathBuf,
    pub config: Mutex<Config>,
    pub peer_registry: PeerRegistry,
    /// Active watchers, keyed by the folder path they watch. Dropping a `FolderWatcher`
    /// stops it, so removing an entry here is how a folder stops being watched.
    pub watchers: Mutex<HashMap<String, FolderWatcher>>,
    pub hostname: String,
    /// Every peer ever discovered on the LAN, kept even after it goes offline — used to
    /// decide who a change should eventually reach via `outbox`.
    pub roster: Roster,
    /// Per-peer queue of messages that couldn't be delivered immediately.
    pub outbox: Outbox,
}
