//! Persisted user settings: which folders to watch, this machine's stable peer id, and
//! the shared secret used to authenticate change messages with other agents.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub peer_id: String,
    /// Symmetric key this agent signs/verifies change messages with. Every machine that
    /// should be able to notify (and be notified by) this one needs the same value here —
    /// set it once on each machine via the settings window, like a Wi-Fi password.
    #[serde(default = "generate_secret")]
    pub shared_secret: String,
    pub watched_folders: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            peer_id: Uuid::new_v4().to_string(),
            shared_secret: generate_secret(),
            watched_folders: Vec::new(),
        }
    }
}

/// 256 bits of randomness, hex-encoded via two concatenated v4 UUIDs — good enough
/// entropy for a locally-typed-in shared secret without pulling in a dedicated CSPRNG
/// crate just for this.
fn generate_secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

impl Config {
    /// Loads config from `path`, falling back to a fresh default if the file is
    /// missing or unreadable (e.g. first run, or a corrupted file).
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|contents| serde_json::from_str(&contents).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, contents)
    }
}

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("network-local-file-manager")
        .join("config.json")
}

/// Directory where per-folder reconciliation manifests are kept (see [`lfsync_core::ManifestStore`]).
pub fn manifests_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("network-local-file-manager")
        .join("manifests")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");

        let mut config = Config::default();
        config
            .watched_folders
            .push("/home/alice/Documents".to_string());
        config.save(&path).unwrap();

        let loaded = Config::load(&path);
        assert_eq!(loaded.peer_id, config.peer_id);
        assert_eq!(loaded.shared_secret, config.shared_secret);
        assert_eq!(loaded.watched_folders, config.watched_folders);
    }

    #[test]
    fn missing_file_yields_a_fresh_default_with_a_secret() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.json");

        let config = Config::load(&path);
        assert!(config.watched_folders.is_empty());
        assert!(!config.shared_secret.is_empty());
    }

    #[test]
    fn old_config_without_a_shared_secret_still_loads_and_gets_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"peer_id":"abc","watched_folders":["/tmp/x"]}"#).unwrap();

        let config = Config::load(&path);
        assert_eq!(config.peer_id, "abc");
        assert_eq!(config.watched_folders, vec!["/tmp/x".to_string()]);
        assert!(
            !config.shared_secret.is_empty(),
            "must backfill a secret for pre-existing configs"
        );
    }
}
