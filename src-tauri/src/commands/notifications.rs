//! Native permission notifications with working click-to-open handling.
//!
//! `tauri-plugin-notification`'s `onAction` never fires on desktop: its
//! `desktop.rs` calls `notify_rust::Notification::show()` and throws the handle
//! away, so no click event is ever emitted. We bypass that here and drive
//! `notify-rust` directly, keeping the handle so we can wait for the click and
//! emit an app event carrying the originating `projectId` / `runId`. The
//! frontend listens for that event and routes to the exact waiting run.

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

/// Event emitted when the user clicks a permission notification. The payload
/// tells the frontend which run to open. Serialized camelCase for JS.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct NotificationClick {
    project_id: String,
    run_id: String,
}

/// Name of the event the frontend listens on.
pub const NOTIFICATION_CLICKED_EVENT: &str = "permission-notification-clicked";

#[cfg(target_os = "macos")]
static SET_APP: std::sync::Once = std::sync::Once::new();

/// Show a native notification for a pending permission/question on a run the
/// user isn't currently viewing. Returns immediately; the blocking wait for the
/// user's click runs on a dedicated OS thread.
#[tauri::command]
pub async fn show_permission_notification(
    app: AppHandle,
    title: String,
    body: String,
    project_id: String,
    run_id: String,
) -> Result<(), String> {
    // macOS needs a bundle identifier to own the notification via
    // NSUserNotificationCenter. Dev builds aren't bundled, so we borrow
    // Terminal's identity exactly like tauri-plugin-notification does.
    #[cfg(target_os = "macos")]
    SET_APP.call_once(|| {
        let ident = if tauri::is_dev() {
            "com.apple.Terminal"
        } else {
            "vn.papay.devdy"
        };
        let _ = notify_rust::set_application(ident);
    });

    // `wait_for_action` blocks until the user interacts AND requires the main
    // run loop (Tauri's event loop) to keep pumping to receive the delegate
    // callback — so it must run on a background thread, never the main one.
    std::thread::spawn(move || {
        let mut notification = notify_rust::Notification::new();
        notification.summary(&title).body(&body);
        #[cfg(target_os = "macos")]
        {
            notification.sound_name("default");
            // Critical: NSUserNotificationCenter only *waits for* (and thus
            // reports) a click when the notification "needs a response" — i.e.
            // it has at least one action/close button. Without this, the
            // notification is sent fire-and-forget and `wait_for_action` returns
            // immediately with no click ever captured. Adding an action makes
            // both the "Open" button AND a plain body click deliver an event.
            notification.action("open", "Open");
        }

        match notification.show() {
            Ok(handle) => {
                handle.wait_for_action(|action| {
                    // "default" = body click, an action id = a button, and
                    // "__closed" = dismissed without acting (ignore that one).
                    if action != "__closed" {
                        // Clicking a notification we handle ourselves does NOT
                        // auto-activate a backgrounded app on macOS, so raise the
                        // main window explicitly before routing.
                        if let Some(win) = app.get_webview_window("main") {
                            let _ = win.unminimize();
                            let _ = win.show();
                            let _ = win.set_focus();
                        }
                        let _ = app.emit(
                            NOTIFICATION_CLICKED_EVENT,
                            NotificationClick {
                                project_id,
                                run_id,
                            },
                        );
                    }
                });
            }
            Err(err) => {
                tracing::warn!("failed to show permission notification: {err}");
            }
        }
    });

    Ok(())
}
