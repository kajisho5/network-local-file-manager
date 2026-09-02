//! A durable memory of every peer this agent has ever discovered on the LAN, independent
//! of whether they're online right now.
//!
//! [`crate::peer::PeerRegistry`] only tracks currently-reachable peers — it forgets a
//! peer the moment mDNS reports it offline. That's fine for deciding who to send to
//! *right now*, but [`crate::outbox::Outbox`] needs to know who to keep queuing messages
//! for even while they're offline, which is what this durable roster is for.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Debug, Default, Serialize, Deserialize)]
struct RosterFile {
    /// peer_id -> last known hostname.
    peers: HashMap<String, String>,
}

/// Thread-safe, cheaply cloneable (backed by an `Arc`) so it can be shared between the
/// mDNS discovery thread and async tasks, the same way [`crate::peer::PeerRegistry`] is.
#[derive(Clone)]
pub struct Roster {
    path: PathBuf,
    peers: Arc<Mutex<HashMap<String, String>>>,
}

impl Roster {
    /// Loads the roster from `path`, or starts empty if it doesn't exist yet / is
    /// unreadable.
    pub fn load(path: PathBuf) -> Self {
        let peers = std::fs::read_to_string(&path)
            .ok()
            .and_then(|contents| serde_json::from_str::<RosterFile>(&contents).ok())
            .map(|file| file.peers)
            .unwrap_or_default();

        Self {
            path,
            peers: Arc::new(Mutex::new(peers)),
        }
    }

    /// Records that `peer_id` (displayed as `hostname`) has been seen. A no-op (no disk
    /// write) if this is already what's on record for that peer.
    pub fn remember(&self, peer_id: &str, hostname: &str) {
        let mut peers = self.peers.lock().unwrap();
        if peers.get(peer_id).map(String::as_str) == Some(hostname) {
            return;
        }
        peers.insert(peer_id.to_string(), hostname.to_string());

        let file = RosterFile {
            peers: peers.clone(),
        };
        drop(peers);
        if let Err(err) = save(&self.path, &file) {
            tracing::warn!(?err, path = %self.path.display(), "failed to persist peer roster");
        }
    }

    /// Every peer_id ever remembered, in no particular order.
    pub fn peer_ids(&self) -> Vec<String> {
        self.peers.lock().unwrap().keys().cloned().collect()
    }
}

fn save(path: &std::path::Path, file: &RosterFile) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let contents = serde_json::to_string_pretty(file).map_err(std::io::Error::other)?;
    std::fs::write(path, contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remember_persists_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("roster.json");

        let roster = Roster::load(path.clone());
        roster.remember("peer-a", "desktop-a");
        roster.remember("peer-b", "laptop-b");

        let reloaded = Roster::load(path);
        let mut ids = reloaded.peer_ids();
        ids.sort();
        assert_eq!(ids, vec!["peer-a".to_string(), "peer-b".to_string()]);
    }

    #[test]
    fn remembering_the_same_hostname_again_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("roster.json");

        let roster = Roster::load(path.clone());
        roster.remember("peer-a", "desktop-a");
        let after_first = std::fs::read_to_string(&path).unwrap();

        roster.remember("peer-a", "desktop-a");
        let after_second = std::fs::read_to_string(&path).unwrap();

        assert_eq!(after_first, after_second);
    }

    #[test]
    fn missing_file_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.json");
        assert!(Roster::load(path).peer_ids().is_empty());
    }
}
