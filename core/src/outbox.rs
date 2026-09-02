//! A persistent, per-peer queue of change messages that couldn't be delivered right away
//! (the peer was offline, or the send failed), so they can be delivered once that peer
//! is seen online again — see [`crate::roster::Roster`] for how "known peers" are tracked
//! even while offline, and [`crate::peer::broadcast_with_outbox`] for where messages get
//! queued here in the first place.
//!
//! This is deliberately simple: one append-only JSON-lines file per peer, no encryption
//! at rest (messages are signed fresh at send time — see [`crate::protocol::SharedSecret`]),
//! bounded by a max queue length and a max age so a permanently-gone peer's queue doesn't
//! grow forever.

use crate::protocol::ChangeMessage;
use chrono::{Duration as ChronoDuration, Utc};
use std::io;
use std::path::PathBuf;

/// Oldest entries are dropped once a peer's queue exceeds this many pending messages.
const MAX_QUEUED_PER_PEER: usize = 500;

/// Entries older than this are dropped the next time that peer's queue is read, on the
/// assumption a peer that's been gone this long probably isn't coming back with this
/// install still around to catch up.
const MAX_AGE_DAYS: i64 = 7;

#[derive(Clone)]
pub struct Outbox {
    dir: PathBuf,
}

impl Outbox {
    pub fn open(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Appends `msg` to `peer_id`'s pending queue, trimming the oldest entries if this
    /// pushes it over [`MAX_QUEUED_PER_PEER`].
    pub fn enqueue(&self, peer_id: &str, msg: &ChangeMessage) -> io::Result<()> {
        let mut pending = self.read_all(peer_id)?;
        pending.push(msg.clone());
        if pending.len() > MAX_QUEUED_PER_PEER {
            let overflow = pending.len() - MAX_QUEUED_PER_PEER;
            pending.drain(0..overflow);
        }
        self.write_all(peer_id, &pending)
    }

    /// Every message currently queued for `peer_id`, oldest first, with anything older
    /// than [`MAX_AGE_DAYS`] pruned out (and that pruning persisted).
    pub fn pending(&self, peer_id: &str) -> io::Result<Vec<ChangeMessage>> {
        let all = self.read_all(peer_id)?;
        let cutoff = Utc::now() - ChronoDuration::days(MAX_AGE_DAYS);
        let fresh_count = all.iter().filter(|m| m.timestamp >= cutoff).count();
        if fresh_count == all.len() {
            return Ok(all);
        }
        let fresh: Vec<_> = all.into_iter().filter(|m| m.timestamp >= cutoff).collect();
        self.write_all(peer_id, &fresh)?;
        Ok(fresh)
    }

    /// Removes the first `count` pending messages for `peer_id` — call this after
    /// successfully delivering that many, in order, from the front of [`Self::pending`].
    pub fn acknowledge(&self, peer_id: &str, count: usize) -> io::Result<()> {
        if count == 0 {
            return Ok(());
        }
        let mut all = self.read_all(peer_id)?;
        let count = count.min(all.len());
        all.drain(0..count);
        self.write_all(peer_id, &all)
    }

    fn file_path(&self, peer_id: &str) -> PathBuf {
        self.dir.join(format!("{peer_id}.jsonl"))
    }

    fn read_all(&self, peer_id: &str) -> io::Result<Vec<ChangeMessage>> {
        let contents = match std::fs::read_to_string(self.file_path(peer_id)) {
            Ok(contents) => contents,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        Ok(contents
            .lines()
            .filter_map(|line| serde_json::from_str::<ChangeMessage>(line).ok())
            .collect())
    }

    fn write_all(&self, peer_id: &str, messages: &[ChangeMessage]) -> io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        if messages.is_empty() {
            // Nothing pending — remove the file rather than leave an empty one behind.
            match std::fs::remove_file(self.file_path(peer_id)) {
                Ok(()) => return Ok(()),
                Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(err) => return Err(err),
            }
        }
        let mut contents = String::new();
        for msg in messages {
            contents.push_str(&serde_json::to_string(msg).map_err(io::Error::other)?);
            contents.push('\n');
        }
        std::fs::write(self.file_path(peer_id), contents)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ChangeKind;
    use chrono::Utc;

    fn msg(path: &str) -> ChangeMessage {
        ChangeMessage {
            peer_id: "sender".into(),
            hostname: "desktop-a".into(),
            path: path.into(),
            kind: ChangeKind::Modified,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn enqueue_pending_acknowledge_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = Outbox::open(dir.path().to_path_buf());

        outbox.enqueue("peer-a", &msg("a.txt")).unwrap();
        outbox.enqueue("peer-a", &msg("b.txt")).unwrap();
        assert_eq!(outbox.pending("peer-a").unwrap().len(), 2);

        outbox.acknowledge("peer-a", 1).unwrap();
        let remaining = outbox.pending("peer-a").unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].path, "b.txt");
    }

    #[test]
    fn unknown_peer_has_no_pending_messages() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = Outbox::open(dir.path().to_path_buf());
        assert!(outbox.pending("nobody").unwrap().is_empty());
    }

    #[test]
    fn enqueue_caps_queue_length_by_dropping_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = Outbox::open(dir.path().to_path_buf());

        for i in 0..(MAX_QUEUED_PER_PEER + 10) {
            outbox
                .enqueue("peer-a", &msg(&format!("file-{i}.txt")))
                .unwrap();
        }

        let pending = outbox.pending("peer-a").unwrap();
        assert_eq!(pending.len(), MAX_QUEUED_PER_PEER);
        // The oldest 10 should have been dropped, so the queue starts at file-10.
        assert_eq!(pending[0].path, "file-10.txt");
    }

    #[test]
    fn pending_prunes_entries_older_than_max_age() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = Outbox::open(dir.path().to_path_buf());

        let mut stale = msg("old.txt");
        stale.timestamp = Utc::now() - ChronoDuration::days(MAX_AGE_DAYS + 1);
        outbox.enqueue("peer-a", &stale).unwrap();
        outbox.enqueue("peer-a", &msg("fresh.txt")).unwrap();

        let pending = outbox.pending("peer-a").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].path, "fresh.txt");
    }

    #[test]
    fn acknowledging_everything_removes_the_queue_file() {
        let dir = tempfile::tempdir().unwrap();
        let outbox = Outbox::open(dir.path().to_path_buf());

        outbox.enqueue("peer-a", &msg("a.txt")).unwrap();
        outbox.acknowledge("peer-a", 1).unwrap();

        assert!(!dir.path().join("peer-a.jsonl").exists());
        assert!(outbox.pending("peer-a").unwrap().is_empty());
    }
}
