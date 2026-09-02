// Suppress the extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod config;
mod notify_toast;
mod state;
mod watch;

use config::{config_path, Config};
use lfsync_core::{
    run_peer_server, ChangeMessage, Outbox, PeerHandle, PeerRegistry, Roster, SharedSecret,
    DEFAULT_PORT,
};
use state::AppState;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, WindowEvent};

fn main() {
    tracing_subscriber::fmt::init();

    let path = config_path();
    let config = Config::load(&path);
    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown-pc".to_string());
    let peer_registry = PeerRegistry::new();
    let roster = Roster::load(config::roster_path());
    let outbox = Outbox::open(config::outbox_dir());

    let app_state = AppState {
        config_path: path,
        config: Mutex::new(config),
        peer_registry: peer_registry.clone(),
        watchers: Mutex::new(HashMap::new()),
        hostname: hostname.clone(),
        roster: roster.clone(),
        outbox: outbox.clone(),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::get_watched_folders,
            commands::add_watched_folder,
            commands::remove_watched_folder,
            commands::get_peers,
            commands::pick_folder,
            commands::get_shared_secret,
            commands::set_shared_secret,
            commands::set_folder_excludes,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let (peer_id, shared_secret) = {
                let state = app.state::<AppState>();
                let config = state.config.lock().unwrap();
                (
                    config.peer_id.clone(),
                    SharedSecret::new(&config.shared_secret),
                )
            };
            let hostname_for_discovery = hostname.clone();
            let registry_for_task = peer_registry.clone();
            let roster_for_task = roster.clone();
            let outbox_for_task = outbox.clone();

            tauri::async_runtime::spawn(async move {
                run_agent_networking(
                    handle,
                    peer_id,
                    hostname_for_discovery,
                    registry_for_task,
                    roster_for_task,
                    outbox_for_task,
                    shared_secret,
                )
                .await;
            });

            let folders = app
                .state::<AppState>()
                .config
                .lock()
                .unwrap()
                .watched_folders
                .clone();
            for folder in folders {
                if let Err(err) = watch::start_watching_folder(app.handle(), &folder) {
                    tracing::error!(folder = %folder.path, %err, "failed to start watching folder");
                }
            }

            build_tray(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            // Keep the agent running in the tray instead of quitting when the settings
            // window is closed; the tray's "終了" menu item is the real quit path.
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Binds the peer TCP server, starts mDNS discovery/advertisement, shows a toast for
/// every change received from a peer, and flushes `outbox` for a peer the moment it's
/// (re)discovered on the LAN — see `lfsync_core::outbox` for what that does and doesn't
/// cover. Runs for the lifetime of the app.
///
/// `shared_secret` is captured once at startup: if the user changes it later via the
/// settings window, this server keeps validating incoming messages against the old value
/// (and outgoing broadcasts pick up the new one immediately) until the app restarts. A
/// mismatch here just means peers stop hearing from each other, not a security hole.
async fn run_agent_networking(
    app: tauri::AppHandle,
    peer_id: String,
    hostname: String,
    registry: PeerRegistry,
    roster: Roster,
    outbox: Outbox,
    shared_secret: SharedSecret,
) {
    // Taken before the peer server setup below, which may move `shared_secret` down one
    // of two branches — this clone stays valid for the outbox-flush task regardless of
    // which branch runs.
    let flush_secret = shared_secret.clone();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ChangeMessage>();

    let default_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), DEFAULT_PORT);
    let local_addr = match run_peer_server(default_addr, shared_secret.clone(), tx.clone()).await {
        Ok((addr, _handle)) => addr,
        Err(err) => {
            tracing::warn!(
                ?err,
                "default port unavailable, binding a random port instead"
            );
            let random_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
            match run_peer_server(random_addr, shared_secret, tx).await {
                Ok((addr, _handle)) => addr,
                Err(err) => {
                    tracing::error!(?err, "failed to start peer server; LAN sync is disabled");
                    return;
                }
            }
        }
    };

    let (peer_online_tx, mut peer_online_rx) = tokio::sync::mpsc::unbounded_channel::<PeerHandle>();

    // Keep the daemon alive for as long as the app runs by leaking it into this task's
    // scope: dropping it would stop advertising/browsing.
    match lfsync_core::start_discovery(
        peer_id,
        hostname,
        local_addr.port(),
        registry,
        roster,
        peer_online_tx,
    ) {
        Ok(daemon) => std::mem::forget(daemon),
        Err(err) => tracing::error!(?err, "failed to start mDNS discovery"),
    }

    tauri::async_runtime::spawn(async move {
        while let Some(peer) = peer_online_rx.recv().await {
            flush_outbox_for(&peer, &outbox, &flush_secret).await;
        }
    });

    while let Some(msg) = rx.recv().await {
        notify_toast::show_change_toast(&app, &msg.hostname, &msg.path, msg.kind);
    }
}

/// Delivers everything queued in `outbox` for `peer`, in order, stopping at the first
/// failed send so a gap doesn't get acknowledged as delivered.
async fn flush_outbox_for(peer: &PeerHandle, outbox: &Outbox, secret: &SharedSecret) {
    let pending = match outbox.pending(&peer.peer_id) {
        Ok(pending) => pending,
        Err(err) => {
            tracing::warn!(?err, peer = %peer.hostname, "failed to read outbox for peer");
            return;
        }
    };
    if pending.is_empty() {
        return;
    }

    let mut delivered = 0;
    for msg in &pending {
        match msg.send_to(peer.addr, secret).await {
            Ok(()) => delivered += 1,
            Err(err) => {
                tracing::warn!(?err, peer = %peer.hostname, "failed to flush a queued change to peer");
                break;
            }
        }
    }

    if delivered > 0 {
        if let Err(err) = outbox.acknowledge(&peer.peer_id, delivered) {
            tracing::warn!(?err, peer = %peer.hostname, "failed to acknowledge flushed outbox entries");
        } else {
            tracing::info!(peer = %peer.hostname, delivered, "delivered queued changes to a peer that came back online");
        }
    }
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let show_item = MenuItem::with_id(app, "show", "開く", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "終了", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show_item, &quit_item])?;

    TrayIconBuilder::new()
        .icon(
            app.default_window_icon()
                .cloned()
                .expect("tray icon asset is missing"),
        )
        .menu(&menu)
        .tooltip("network-local-file-manager")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;

    Ok(())
}
