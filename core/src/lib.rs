//! Core logic for the local-network file manager: watches folders for changes, discovers
//! peer agents on the LAN via mDNS, and exchanges change notifications over TCP.
//!
//! This crate has no GUI dependencies so it can be built and tested without a display
//! server; the Tauri app in `src-tauri` wires it up to a system tray and native
//! notifications.

pub mod discovery;
pub mod peer;
pub mod protocol;
pub mod watcher;

pub use discovery::start as start_discovery;
pub use peer::{broadcast, run_peer_server, PeerHandle, PeerRegistry};
pub use protocol::{ChangeKind, ChangeMessage, DEFAULT_PORT};
pub use watcher::{ChangeEvent, FolderWatcher};
