//! LAN peer discovery via mDNS, so agents can find each other without any central server.

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::net::SocketAddr;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::peer::{PeerHandle, PeerRegistry};
use crate::roster::Roster;

/// mDNS service type advertised by every agent instance.
pub const SERVICE_TYPE: &str = "_lfsync._tcp.local.";

/// Advertises this agent on the LAN and browses for other agents, keeping `registry`
/// up to date as peers appear and disappear.
///
/// Takes an existing `registry` (rather than creating one) so callers can share it with
/// other components, such as the code that broadcasts local changes to known peers.
/// Every newly-resolved peer is also remembered in `roster` (durable even once that peer
/// goes offline again) and sent on `peer_online_tx`, so a caller can react to a peer
/// coming online — e.g. to flush a pending [`crate::outbox::Outbox`] for it. Send errors
/// on that channel (no receiver listening) are ignored.
///
/// `peer_id` does not need to be stable across runs, but must be unique among
/// concurrently running agents (a random UUID is a good choice).
pub fn start(
    peer_id: String,
    hostname: String,
    port: u16,
    registry: PeerRegistry,
    roster: Roster,
    peer_online_tx: mpsc::UnboundedSender<PeerHandle>,
) -> mdns_sd::Result<ServiceDaemon> {
    let daemon = ServiceDaemon::new()?;

    let instance_name = format!("{hostname}-{peer_id}");
    let host_fqdn = format!("{hostname}.local.");
    let properties = [("peer_id", peer_id.as_str())];

    let service_info = ServiceInfo::new(
        SERVICE_TYPE,
        &instance_name,
        &host_fqdn,
        "",
        port,
        &properties[..],
    )?
    .enable_addr_auto();
    daemon.register(service_info)?;
    info!(%instance_name, port, "advertising on LAN via mDNS");

    let receiver = daemon.browse(SERVICE_TYPE)?;
    let watch_registry = registry.clone();
    let own_peer_id = peer_id.clone();
    std::thread::spawn(move || {
        // Maps mDNS fullname -> peer_id, since `ServiceRemoved` only carries the fullname.
        let mut fullname_to_peer_id = std::collections::HashMap::new();
        while let Ok(event) = receiver.recv() {
            handle_event(
                event,
                &watch_registry,
                &roster,
                &peer_online_tx,
                &own_peer_id,
                &mut fullname_to_peer_id,
            );
        }
    });

    Ok(daemon)
}

fn handle_event(
    event: ServiceEvent,
    registry: &PeerRegistry,
    roster: &Roster,
    peer_online_tx: &mpsc::UnboundedSender<PeerHandle>,
    own_peer_id: &str,
    fullname_to_peer_id: &mut std::collections::HashMap<String, String>,
) {
    match event {
        ServiceEvent::ServiceResolved(info) => {
            let Some(remote_peer_id) = info.get_property_val_str("peer_id") else {
                warn!("resolved peer without a peer_id property, ignoring");
                return;
            };
            if remote_peer_id == own_peer_id {
                return;
            }
            let Some(addr) = info.get_addresses().iter().next() else {
                warn!(%remote_peer_id, "resolved peer without any address, ignoring");
                return;
            };
            let socket_addr = SocketAddr::new(*addr, info.get_port());
            let hostname = info.get_hostname().trim_end_matches(".local.").to_string();
            debug!(%remote_peer_id, %hostname, %socket_addr, "discovered peer");
            fullname_to_peer_id.insert(info.get_fullname().to_string(), remote_peer_id.to_string());
            registry.upsert(remote_peer_id, hostname.clone(), socket_addr);
            roster.remember(remote_peer_id, &hostname);
            let _ = peer_online_tx.send(PeerHandle {
                peer_id: remote_peer_id.to_string(),
                hostname,
                addr: socket_addr,
            });
        }
        ServiceEvent::ServiceRemoved(_, fullname) => {
            if let Some(peer_id) = fullname_to_peer_id.remove(&fullname) {
                debug!(%peer_id, "peer went offline");
                registry.remove(&peer_id);
            }
        }
        _ => {}
    }
}
