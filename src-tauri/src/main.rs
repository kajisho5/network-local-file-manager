// Suppress the extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod config;
mod notify_toast;
mod state;
mod watch;

use config::{config_path, Config};
use lfsync_core::{run_peer_server, ChangeMessage, PeerRegistry, DEFAULT_PORT};
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

    let app_state = AppState {
        config_path: path,
        config: Mutex::new(config),
        peer_registry: peer_registry.clone(),
        watchers: Mutex::new(HashMap::new()),
        hostname: hostname.clone(),
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
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let peer_id = app
                .state::<AppState>()
                .config
                .lock()
                .unwrap()
                .peer_id
                .clone();
            let hostname_for_discovery = hostname.clone();
            let registry_for_task = peer_registry.clone();

            tauri::async_runtime::spawn(async move {
                run_agent_networking(handle, peer_id, hostname_for_discovery, registry_for_task)
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
                    tracing::error!(%folder, %err, "failed to start watching folder");
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

/// Binds the peer TCP server, starts mDNS discovery/advertisement, and shows a toast for
/// every change received from a peer. Runs for the lifetime of the app.
async fn run_agent_networking(
    app: tauri::AppHandle,
    peer_id: String,
    hostname: String,
    registry: PeerRegistry,
) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ChangeMessage>();

    let default_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), DEFAULT_PORT);
    let local_addr = match run_peer_server(default_addr, tx.clone()).await {
        Ok((addr, _handle)) => addr,
        Err(err) => {
            tracing::warn!(
                ?err,
                "default port unavailable, binding a random port instead"
            );
            let random_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
            match run_peer_server(random_addr, tx).await {
                Ok((addr, _handle)) => addr,
                Err(err) => {
                    tracing::error!(?err, "failed to start peer server; LAN sync is disabled");
                    return;
                }
            }
        }
    };

    // Keep the daemon alive for as long as the app runs by leaking it into this task's
    // scope: dropping it would stop advertising/browsing.
    match lfsync_core::start_discovery(peer_id, hostname, local_addr.port(), registry) {
        Ok(daemon) => std::mem::forget(daemon),
        Err(err) => tracing::error!(?err, "failed to start mDNS discovery"),
    }

    while let Some(msg) = rx.recv().await {
        notify_toast::show_change_toast(&app, &msg.hostname, &msg.path, msg.kind);
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
