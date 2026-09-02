//! Wire format for change events exchanged between peers on the LAN.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// The default TCP port agents listen on for incoming change events.
pub const DEFAULT_PORT: u16 = 47821;

/// The kind of filesystem change that occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Created,
    Modified,
    Removed,
    Renamed,
}

/// A single change event, broadcast to peers after being detected locally.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeMessage {
    /// Stable identifier of the agent that observed the change.
    pub peer_id: String,
    /// Human-readable hostname of the machine that observed the change, for display.
    pub hostname: String,
    /// Path relative to the watched folder's root, using forward slashes.
    pub path: String,
    pub kind: ChangeKind,
    pub timestamp: DateTime<Utc>,
}

impl ChangeMessage {
    /// Serializes as a single line of JSON (no embedded newlines) for newline-delimited framing.
    pub fn to_line(&self) -> io::Result<String> {
        let mut line = serde_json::to_string(self).map_err(io::Error::other)?;
        line.push('\n');
        Ok(line)
    }

    pub fn from_line(line: &str) -> io::Result<Self> {
        serde_json::from_str(line.trim_end()).map_err(io::Error::other)
    }

    /// Sends this message to a peer over a fresh TCP connection.
    pub async fn send_to(&self, addr: std::net::SocketAddr) -> io::Result<()> {
        let mut stream = TcpStream::connect(addr).await?;
        stream.write_all(self.to_line()?.as_bytes()).await?;
        stream.flush().await
    }

    /// Reads a single newline-delimited `ChangeMessage` from a stream.
    pub async fn read_from(stream: &mut TcpStream) -> io::Result<Self> {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed",
            ));
        }
        Self::from_line(&line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json_line() {
        let msg = ChangeMessage {
            peer_id: "peer-1".into(),
            hostname: "desktop-a".into(),
            path: "docs/report.docx".into(),
            kind: ChangeKind::Modified,
            timestamp: Utc::now(),
        };
        let line = msg.to_line().unwrap();
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1);

        let parsed = ChangeMessage::from_line(&line).unwrap();
        assert_eq!(parsed, msg);
    }
}
