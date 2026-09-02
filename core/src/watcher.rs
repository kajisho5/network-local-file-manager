//! Recursive folder watching with debouncing, built on top of the `notify` crate.

use crate::protocol::ChangeKind;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// A debounced, classified filesystem change under a watched root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeEvent {
    pub path: PathBuf,
    pub kind: ChangeKind,
}

/// Watches a single folder tree and emits debounced [`ChangeEvent`]s on `sender`.
///
/// Dropping this value stops the watch.
pub struct FolderWatcher {
    _watcher: RecommendedWatcher,
}

impl FolderWatcher {
    pub fn watch(
        root: impl AsRef<Path>,
        debounce: Duration,
        sender: mpsc::UnboundedSender<ChangeEvent>,
    ) -> notify::Result<Self> {
        let (std_tx, std_rx) = std_mpsc::channel::<notify::Result<Event>>();
        let mut watcher = notify::recommended_watcher(move |res| {
            let _ = std_tx.send(res);
        })?;
        watcher.watch(root.as_ref(), RecursiveMode::Recursive)?;

        std::thread::spawn(move || debounce_loop(std_rx, debounce, sender));

        Ok(Self { _watcher: watcher })
    }
}

/// Bridges the raw `notify` callback thread into a debounced stream of [`ChangeEvent`]s.
///
/// Rapid-fire events for the same path (e.g. a save that triggers several writes) are
/// coalesced into one event, emitted once the path has been quiet for `debounce`.
fn debounce_loop(
    std_rx: std_mpsc::Receiver<notify::Result<Event>>,
    debounce: Duration,
    sender: mpsc::UnboundedSender<ChangeEvent>,
) {
    let mut pending: HashMap<PathBuf, (ChangeKind, Instant)> = HashMap::new();

    loop {
        match std_rx.recv_timeout(debounce) {
            Ok(Ok(event)) => {
                if let Some(kind) = classify(&event.kind) {
                    let now = Instant::now();
                    for path in event.paths {
                        pending
                            .entry(path)
                            .and_modify(|(existing, seen_at)| {
                                *existing = merge_kind(*existing, kind);
                                *seen_at = now;
                            })
                            .or_insert((kind, now));
                    }
                }
            }
            Ok(Err(_)) => {}
            Err(std_mpsc::RecvTimeoutError::Timeout) => {}
            Err(std_mpsc::RecvTimeoutError::Disconnected) => {
                flush_all(&mut pending, &sender);
                return;
            }
        }

        let now = Instant::now();
        let ready: Vec<PathBuf> = pending
            .iter()
            .filter(|(_, (_, seen_at))| now.duration_since(*seen_at) >= debounce)
            .map(|(path, _)| path.clone())
            .collect();

        for path in ready {
            if let Some((kind, _)) = pending.remove(&path) {
                if sender.send(ChangeEvent { path, kind }).is_err() {
                    return;
                }
            }
        }
    }
}

fn flush_all(
    pending: &mut HashMap<PathBuf, (ChangeKind, Instant)>,
    sender: &mpsc::UnboundedSender<ChangeEvent>,
) {
    for (path, (kind, _)) in pending.drain() {
        let _ = sender.send(ChangeEvent { path, kind });
    }
}

/// Combines two change kinds seen for the same path within one debounce window.
///
/// A create followed by the modify events that naturally happen while a file is being
/// written (most editors write-then-close) should still be reported as `Created` — the
/// interesting fact is that the file is new, not that its write completed in two syscalls.
/// A later `Removed` always wins, since that reflects the path's true final state.
fn merge_kind(existing: ChangeKind, incoming: ChangeKind) -> ChangeKind {
    match incoming {
        ChangeKind::Removed => ChangeKind::Removed,
        ChangeKind::Created => ChangeKind::Created,
        ChangeKind::Modified if existing == ChangeKind::Created => ChangeKind::Created,
        other => other,
    }
}

fn classify(kind: &EventKind) -> Option<ChangeKind> {
    match kind {
        EventKind::Create(_) => Some(ChangeKind::Created),
        EventKind::Modify(_) => Some(ChangeKind::Modified),
        EventKind::Remove(_) => Some(ChangeKind::Removed),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration as StdDuration;
    use tokio::time::timeout;

    #[tokio::test]
    async fn detects_file_creation() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let _watcher = FolderWatcher::watch(dir.path(), StdDuration::from_millis(200), tx).unwrap();

        // give the watcher time to register before we touch the filesystem
        tokio::time::sleep(StdDuration::from_millis(200)).await;

        let file_path = dir.path().join("new_file.txt");
        fs::write(&file_path, b"hello").unwrap();

        let event = timeout(StdDuration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for change event")
            .expect("channel closed unexpectedly");

        assert_eq!(event.path, file_path);
        assert_eq!(event.kind, ChangeKind::Created);
    }

    #[tokio::test]
    async fn detects_atomic_replace_via_rename() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("report.docx");
        fs::write(&file_path, b"original").unwrap();

        let (tx, mut rx) = mpsc::unbounded_channel();
        let _watcher = FolderWatcher::watch(dir.path(), StdDuration::from_millis(200), tx).unwrap();
        tokio::time::sleep(StdDuration::from_millis(200)).await;

        // Simulate an atomic "replace" save: write to a temp file, then rename over the
        // original. This is what most editors and sync tools do to avoid partial writes;
        // the OS reports it as the destination path being modified in place. The temp
        // path itself also generates an event (it briefly existed), so scan for the one
        // we actually care about rather than assuming arrival order.
        let tmp_path = dir.path().join("report.docx.tmp");
        fs::write(&tmp_path, b"replaced").unwrap();
        fs::rename(&tmp_path, &file_path).unwrap();

        let event = timeout(StdDuration::from_secs(5), async {
            loop {
                let event = rx.recv().await.expect("channel closed unexpectedly");
                if event.path == file_path {
                    return event;
                }
            }
        })
        .await
        .expect("timed out waiting for change event on destination path");

        assert_eq!(event.kind, ChangeKind::Modified);
    }
}
