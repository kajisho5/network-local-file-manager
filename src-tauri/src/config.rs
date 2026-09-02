//! Persisted user settings: which folders to watch, and this machine's stable peer id.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub peer_id: String,
    pub watched_folders: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            peer_id: uuid::Uuid::new_v4().to_string(),
            watched_folders: Vec::new(),
        }
    }
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
