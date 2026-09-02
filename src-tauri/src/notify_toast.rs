//! Turns a detected change into a native OS toast notification.

use lfsync_core::ChangeKind;
use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

fn verb(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Created => "作成されました",
        ChangeKind::Modified => "更新されました",
        ChangeKind::Removed => "削除されました",
        ChangeKind::Renamed => "名前が変更されました",
    }
}

/// Shows a toast for a change to `path`, attributed to `actor_label` (e.g. a hostname,
/// or "このPC" for a change this machine detected itself).
pub fn show_change_toast(app: &AppHandle, actor_label: &str, path: &str, kind: ChangeKind) {
    let title = actor_label.to_string();
    let body = format!("{path} が{}", verb(kind));

    if let Err(err) = app.notification().builder().title(title).body(body).show() {
        tracing::warn!(?err, "failed to show notification");
    }
}
