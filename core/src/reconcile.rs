//! Startup reconciliation: catches this machine up on changes made to a watched folder
//! while its agent wasn't running (app closed, PC asleep, etc.), by comparing a fresh
//! directory scan against the manifest saved the last time it was watched.
//!
//! This only helps the machine doing the reconciling learn what changed on its own disk
//! while it was offline — it does **not** backfill peers about changes they missed while
//! *they* were offline. Real cross-machine backfill would need a persistent per-peer
//! outbox and isn't implemented yet.

use crate::exclude::ExcludeRules;
use crate::protocol::ChangeKind;
use crate::watcher::ChangeEvent;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

/// A lightweight fingerprint of one file, cheap enough to keep for every file in a
/// watched tree without hashing file contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct FileStamp {
    size: u64,
    modified_unix_ms: i64,
}

impl FileStamp {
    fn from_metadata(metadata: &Metadata) -> Self {
        let modified_unix_ms = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        Self {
            size: metadata.len(),
            modified_unix_ms,
        }
    }
}

/// A snapshot of every non-excluded file under a watched root, keyed by path relative to
/// that root (forward-slash separated, matching [`crate::protocol::ChangeMessage::path`]).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Manifest {
    entries: HashMap<String, FileStamp>,
}

impl Manifest {
    fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|contents| serde_json::from_str(&contents).ok())
            .unwrap_or_default()
    }

    fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = serde_json::to_string(self).map_err(std::io::Error::other)?;
        std::fs::write(path, contents)
    }
}

/// Walks `root`, skipping anything `excludes` matches, and builds a fresh [`Manifest`].
fn scan(root: &Path, excludes: &ExcludeRules) -> Manifest {
    let mut entries = HashMap::new();
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if excludes.is_excluded(path) {
            continue;
        }
        let (Ok(metadata), Ok(relative)) = (entry.metadata(), path.strip_prefix(root)) else {
            continue;
        };
        let key = relative.to_string_lossy().replace('\\', "/");
        entries.insert(key, FileStamp::from_metadata(&metadata));
    }
    Manifest { entries }
}

/// Compares two manifests and returns the [`ChangeEvent`]s needed to explain the
/// difference: new or changed files as `Created`/`Modified`, vanished files as `Removed`.
fn diff(root: &Path, old: &Manifest, new: &Manifest) -> Vec<ChangeEvent> {
    let mut events = Vec::new();

    for (relative, stamp) in &new.entries {
        let kind = match old.entries.get(relative) {
            None => Some(ChangeKind::Created),
            Some(old_stamp) if old_stamp != stamp => Some(ChangeKind::Modified),
            _ => None,
        };
        if let Some(kind) = kind {
            events.push(ChangeEvent {
                path: root.join(relative),
                kind,
            });
        }
    }
    for relative in old.entries.keys() {
        if !new.entries.contains_key(relative) {
            events.push(ChangeEvent {
                path: root.join(relative),
                kind: ChangeKind::Removed,
            });
        }
    }

    events
}

/// Keeps a watched folder's on-disk manifest in sync with reality, so that a restart can
/// tell what changed while the agent was down.
pub struct ManifestStore {
    root: PathBuf,
    manifest_path: PathBuf,
    manifest: Mutex<Manifest>,
}

impl ManifestStore {
    /// Loads the manifest saved last time `root` was watched, diffs it against the
    /// folder's current state, and persists the fresh baseline. Returns the store (to
    /// keep the manifest updated as live changes arrive) plus any catch-up events found —
    /// changes that happened while nothing was watching this folder.
    ///
    /// The very first time a folder is watched there is no prior manifest to diff
    /// against; that case reports no catch-up events rather than treating every
    /// pre-existing file as newly "created" (which would flood a large existing folder
    /// with toasts the moment it's added).
    pub fn open(
        root: PathBuf,
        manifest_path: PathBuf,
        excludes: &ExcludeRules,
    ) -> (Self, Vec<ChangeEvent>) {
        let manifest_existed = manifest_path.exists();
        let old = Manifest::load(&manifest_path);
        let fresh = scan(&root, excludes);
        let events = if manifest_existed {
            diff(&root, &old, &fresh)
        } else {
            Vec::new()
        };

        if let Err(err) = fresh.save(&manifest_path) {
            tracing::warn!(?err, path = %manifest_path.display(), "failed to persist reconciliation manifest");
        }

        let store = Self {
            root,
            manifest_path,
            manifest: Mutex::new(fresh),
        };
        (store, events)
    }

    /// Updates the manifest for one change already reported by the live watcher, and
    /// persists it — so a later restart's catch-up scan doesn't re-report it.
    pub fn record(&self, event: &ChangeEvent) {
        let Ok(relative) = event.path.strip_prefix(&self.root) else {
            return;
        };
        let key = relative.to_string_lossy().replace('\\', "/");

        let mut manifest = self.manifest.lock().unwrap();
        match event.kind {
            ChangeKind::Removed => {
                manifest.entries.remove(&key);
            }
            _ => match std::fs::metadata(&event.path) {
                Ok(metadata) if metadata.is_file() => {
                    manifest
                        .entries
                        .insert(key, FileStamp::from_metadata(&metadata));
                }
                // The file is already gone again, or isn't a plain file (e.g. a
                // directory create) — nothing meaningful to record.
                _ => return,
            },
        }

        if let Err(err) = manifest.save(&self.manifest_path) {
            tracing::warn!(?err, "failed to persist manifest update");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn catches_up_on_changes_made_while_unwatched() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("manifest.json");
        let root = dir.path().join("watched");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("keep.txt"), b"unchanged").unwrap();
        fs::write(root.join("will_change.txt"), b"before").unwrap();
        fs::write(root.join("will_be_removed.txt"), b"bye").unwrap();

        // First run: establishes the baseline manifest, no catch-up events yet.
        let (store, events) = ManifestStore::open(
            root.clone(),
            manifest_path.clone(),
            &ExcludeRules::default(),
        );
        assert!(events.is_empty());
        drop(store);

        // Simulate changes happening while the agent wasn't running.
        fs::write(root.join("will_change.txt"), b"after, much longer content").unwrap();
        fs::remove_file(root.join("will_be_removed.txt")).unwrap();
        fs::write(root.join("new_file.txt"), b"brand new").unwrap();

        let (_store, events) =
            ManifestStore::open(root.clone(), manifest_path, &ExcludeRules::default());

        let find = |name: &str| events.iter().find(|e| e.path.ends_with(name));
        assert_eq!(
            find("keep.txt"),
            None,
            "unchanged file must not be reported"
        );
        assert_eq!(
            find("will_change.txt").map(|e| e.kind),
            Some(ChangeKind::Modified)
        );
        assert_eq!(
            find("will_be_removed.txt").map(|e| e.kind),
            Some(ChangeKind::Removed)
        );
        assert_eq!(
            find("new_file.txt").map(|e| e.kind),
            Some(ChangeKind::Created)
        );
    }

    #[test]
    fn recorded_live_changes_are_not_reported_again_on_restart() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("manifest.json");
        let root = dir.path().join("watched");
        fs::create_dir(&root).unwrap();

        let (store, events) = ManifestStore::open(
            root.clone(),
            manifest_path.clone(),
            &ExcludeRules::default(),
        );
        assert!(events.is_empty());

        // A live watcher would report this and call `record` for it.
        let new_path = root.join("live_change.txt");
        fs::write(&new_path, b"created while watching").unwrap();
        store.record(&ChangeEvent {
            path: new_path,
            kind: ChangeKind::Created,
        });
        drop(store);

        let (_store, events) = ManifestStore::open(root, manifest_path, &ExcludeRules::default());
        assert!(
            events.is_empty(),
            "a change already recorded live must not resurface as a catch-up event: {events:?}"
        );
    }
}
