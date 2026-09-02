//! Wire format for change events exchanged between peers on the LAN.
//!
//! Every message is authenticated with an HMAC over a shared secret so that only agents
//! configured with the same secret (set once by the user, like a Wi-Fi password) can
//! make each other show notifications. Anyone else on the LAN can still see that traffic
//! is flowing on [`DEFAULT_PORT`], but cannot forge or read a peer's change history from
//! it without the secret, and a receiver silently drops anything that doesn't verify.

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::io;
use std::net::SocketAddr;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

type HmacSha256 = Hmac<Sha256>;

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

/// A symmetric key shared by every agent that should be able to talk to this one.
///
/// Cloning is cheap (an `Arc`-free byte copy is fine here; secrets are short).
#[derive(Clone)]
pub struct SharedSecret(Vec<u8>);

impl SharedSecret {
    pub fn new(secret: impl AsRef<str>) -> Self {
        Self(secret.as_ref().as_bytes().to_vec())
    }

    fn mac_hex(&self, payload: &str) -> String {
        let mut mac =
            HmacSha256::new_from_slice(&self.0).expect("HMAC accepts a key of any length");
        mac.update(payload.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    /// Verifies `mac_hex` against `payload` in constant time (via `hmac`'s `verify_slice`),
    /// so a mismatched secret can't be brute-forced by timing the failure.
    fn verify(&self, payload: &str, mac_hex: &str) -> bool {
        let Ok(expected) = hex::decode(mac_hex) else {
            return false;
        };
        let mut mac =
            HmacSha256::new_from_slice(&self.0).expect("HMAC accepts a key of any length");
        mac.update(payload.as_bytes());
        mac.verify_slice(&expected).is_ok()
    }
}

/// The wire format: a JSON-encoded [`ChangeMessage`] plus an HMAC over its exact bytes.
///
/// The MAC covers `payload` as a string (not the parsed struct), so verification doesn't
/// depend on both sides re-serializing identically.
#[derive(Debug, Serialize, Deserialize)]
struct SignedEnvelope {
    payload: String,
    mac: String,
}

impl ChangeMessage {
    /// Signs this message and serializes it as a single line of JSON (no embedded
    /// newlines) for newline-delimited framing.
    pub fn sign(&self, secret: &SharedSecret) -> io::Result<String> {
        let payload = serde_json::to_string(self).map_err(io::Error::other)?;
        let mac = secret.mac_hex(&payload);
        let mut line =
            serde_json::to_string(&SignedEnvelope { payload, mac }).map_err(io::Error::other)?;
        line.push('\n');
        Ok(line)
    }

    /// Verifies a signed line against `secret` and parses the message inside it.
    ///
    /// Returns an `InvalidData` error if the MAC doesn't match — either the line was
    /// tampered with, or the sender is using a different shared secret.
    pub fn verify_and_parse(line: &str, secret: &SharedSecret) -> io::Result<Self> {
        let envelope: SignedEnvelope =
            serde_json::from_str(line.trim_end()).map_err(io::Error::other)?;
        if !secret.verify(&envelope.payload, &envelope.mac) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "change message failed signature verification (mismatched shared key?)",
            ));
        }
        serde_json::from_str(&envelope.payload).map_err(io::Error::other)
    }

    /// Signs and sends this message to a peer over a fresh TCP connection.
    pub async fn send_to(&self, addr: SocketAddr, secret: &SharedSecret) -> io::Result<()> {
        let line = self.sign(secret)?;
        let mut stream = TcpStream::connect(addr).await?;
        stream.write_all(line.as_bytes()).await?;
        stream.flush().await
    }

    /// Reads a single newline-delimited, signed `ChangeMessage` from a stream.
    pub async fn read_from(stream: &mut TcpStream, secret: &SharedSecret) -> io::Result<Self> {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed",
            ));
        }
        Self::verify_and_parse(&line, secret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ChangeMessage {
        ChangeMessage {
            peer_id: "peer-1".into(),
            hostname: "desktop-a".into(),
            path: "docs/report.docx".into(),
            kind: ChangeKind::Modified,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn round_trips_with_matching_secret() {
        let secret = SharedSecret::new("correct horse battery staple");
        let msg = sample();

        let line = msg.sign(&secret).unwrap();
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1);

        let parsed = ChangeMessage::verify_and_parse(&line, &secret).unwrap();
        assert_eq!(parsed, msg);
    }

    #[test]
    fn rejects_mismatched_secret() {
        let msg = sample();
        let line = msg.sign(&SharedSecret::new("secret-a")).unwrap();

        let err = ChangeMessage::verify_and_parse(&line, &SharedSecret::new("secret-b"))
            .expect_err("wrong secret must not verify");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn rejects_tampered_payload() {
        let secret = SharedSecret::new("correct horse battery staple");
        let line = sample().sign(&secret).unwrap();

        let tampered = line.replace("desktop-a", "desktop-evil");
        assert_ne!(tampered, line);

        let err = ChangeMessage::verify_and_parse(&tampered, &secret)
            .expect_err("tampered payload must not verify");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
