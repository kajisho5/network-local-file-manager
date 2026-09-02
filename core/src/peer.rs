//! TCP transport for exchanging [`ChangeMessage`]s with peers on the LAN.

use crate::protocol::ChangeMessage;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// A discovered peer's address and display name.
#[derive(Debug, Clone, PartialEq)]
pub struct PeerHandle {
    pub peer_id: String,
    pub hostname: String,
    pub addr: SocketAddr,
}

/// A live, thread-safe registry of known peers, keyed by their stable peer id.
///
/// Populated by [`crate::discovery`] and consumed when broadcasting local changes.
#[derive(Clone, Default)]
pub struct PeerRegistry {
    inner: Arc<RwLock<HashMap<String, PeerHandle>>>,
}

impl PeerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert(
        &self,
        peer_id: impl Into<String>,
        hostname: impl Into<String>,
        addr: SocketAddr,
    ) {
        let peer_id = peer_id.into();
        self.inner.write().unwrap().insert(
            peer_id.clone(),
            PeerHandle {
                peer_id,
                hostname: hostname.into(),
                addr,
            },
        );
    }

    pub fn remove(&self, peer_id: &str) {
        self.inner.write().unwrap().remove(peer_id);
    }

    pub fn handles(&self) -> Vec<PeerHandle> {
        self.inner.read().unwrap().values().cloned().collect()
    }

    pub fn addrs(&self) -> Vec<SocketAddr> {
        self.inner
            .read()
            .unwrap()
            .values()
            .map(|h| h.addr)
            .collect()
    }

    pub fn len(&self) -> usize {
        self.inner.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Listens for incoming [`ChangeMessage`]s from peers and forwards each to `sender`.
///
/// Runs until the returned `JoinHandle` is aborted or the process exits.
pub async fn run_peer_server(
    bind_addr: SocketAddr,
    sender: mpsc::UnboundedSender<ChangeMessage>,
) -> std::io::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind(bind_addr).await?;
    let local_addr = listener.local_addr()?;

    let handle = tokio::spawn(async move {
        loop {
            let (mut stream, peer_addr) = match listener.accept().await {
                Ok(pair) => pair,
                Err(err) => {
                    warn!(?err, "failed to accept peer connection");
                    continue;
                }
            };
            let sender = sender.clone();
            tokio::spawn(async move {
                match ChangeMessage::read_from(&mut stream).await {
                    Ok(msg) => {
                        debug!(?peer_addr, ?msg, "received change message");
                        let _ = sender.send(msg);
                    }
                    Err(err) => {
                        warn!(?peer_addr, ?err, "failed to read change message");
                    }
                }
            });
        }
    });

    Ok((local_addr, handle))
}

/// Broadcasts a change to every peer currently in `registry`.
///
/// Best-effort: a peer that is offline or unreachable is skipped without failing the
/// whole broadcast, since LAN peers routinely come and go.
pub async fn broadcast(registry: &PeerRegistry, msg: &ChangeMessage) {
    for addr in registry.addrs() {
        if let Err(err) = msg.send_to(addr).await {
            warn!(?addr, ?err, "failed to send change message to peer");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ChangeKind;
    use chrono::Utc;
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::Duration;
    use tokio::time::timeout;

    #[tokio::test]
    async fn server_forwards_received_message() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let bind_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let (local_addr, _handle) = run_peer_server(bind_addr, tx).await.unwrap();

        let registry = PeerRegistry::new();
        registry.upsert("peer-a", "laptop", local_addr);

        let msg = ChangeMessage {
            peer_id: "peer-a".into(),
            hostname: "laptop".into(),
            path: "notes.txt".into(),
            kind: ChangeKind::Modified,
            timestamp: Utc::now(),
        };
        broadcast(&registry, &msg).await;

        let received = timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for message")
            .expect("channel closed");

        assert_eq!(received, msg);
    }
}
