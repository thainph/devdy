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

/// Event emitted when the user clicks a permission or run-finished notification. The payload
/// tells the frontend which run to open. Serialized camelCase for JS.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct NotificationClick {
    project_id: String,
    run_id: String,
}

/// Name of the event the frontend listens on.
pub const NOTIFICATION_CLICKED_EVENT: &str = "permission-notification-clicked";

/// Emitted when the user clicks a calendar-reminder notification. Payload tells
/// the frontend which event to open.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReminderClick {
    event_id: String,
}

/// Name of the event the frontend listens on for calendar reminders.
pub const CALENDAR_REMINDER_CLICKED_EVENT: &str = "calendar-reminder-clicked";

#[cfg(target_os = "macos")]
static SET_APP: std::sync::Once = std::sync::Once::new();

/// Most notifications kept waiting for a click at once. Each waiter makes
/// `mac-notification-sys` install a 0.5 s timer on the MAIN run loop that does a
/// synchronous XPC round-trip (`deliveredNotifications`) until the notification
/// leaves Notification Center — and banners stay there until the user clears
/// them. With many runs these piled up into the hundreds and starved the UI
/// thread, so past this cap the oldest are withdrawn (which ends their wait as
/// a plain dismissal).
const MAX_WAITING: usize = 5;

/// A notification still waiting for its click, identified by what it shows
/// (the crate's own identifier isn't exposed).
#[derive(Clone, Debug, PartialEq)]
struct Waiting {
    id: u64,
    title: String,
    body: String,
}

static WAITING: std::sync::Mutex<std::collections::VecDeque<Waiting>> =
    std::sync::Mutex::new(std::collections::VecDeque::new());
static NEXT_WAITING_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Queue `entry` and return (copies of) the entries past `cap`, oldest first.
/// They stay queued until their waiter actually ends (`unregister_waiting`), so
/// one not yet delivered when withdrawn — or spared because another with the
/// same text was removed instead — is simply retried on the next call.
fn push_capped(
    queue: &mut std::collections::VecDeque<Waiting>,
    entry: Waiting,
    cap: usize,
) -> Vec<Waiting> {
    queue.push_back(entry);
    let excess = queue.len().saturating_sub(cap);
    queue.iter().take(excess).cloned().collect()
}

/// Track a new waiter, withdrawing the oldest ones past `MAX_WAITING`.
fn register_waiting(app: &AppHandle, title: &str, body: &str) -> u64 {
    let id = NEXT_WAITING_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let evicted = {
        let mut queue = WAITING.lock().unwrap_or_else(|e| e.into_inner());
        let entry = Waiting {
            id,
            title: title.to_string(),
            body: body.to_string(),
        };
        push_capped(&mut queue, entry, MAX_WAITING)
    };
    withdraw(app, evicted);
    id
}

fn unregister_waiting(id: u64) {
    let mut queue = WAITING.lock().unwrap_or_else(|e| e.into_inner());
    unregister_from(&mut queue, id);
}

fn unregister_from(queue: &mut std::collections::VecDeque<Waiting>, id: u64) {
    queue.retain(|w| w.id != id);
}

/// Withdraw every notification still waiting for a click. Called when the main
/// window gains focus: the user is back in the app, where the same state
/// (pending permission, finished run…) is already shown.
pub(crate) fn withdraw_all(app: &AppHandle) {
    let all: Vec<Waiting> = {
        let queue = WAITING.lock().unwrap_or_else(|e| e.into_inner());
        queue.iter().cloned().collect()
    };
    withdraw(app, all);
}

/// Remove `entries` from Notification Center on the main thread (where the
/// crate's poll timers also run); each one's waiter then returns `__closed`.
fn withdraw(app: &AppHandle, entries: Vec<Waiting>) {
    if entries.is_empty() {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let _ = app.run_on_main_thread(move || remove_delivered(entries));
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, entries);
    }
}

#[cfg(target_os = "macos")]
#[allow(deprecated)]
fn remove_delivered(mut entries: Vec<Waiting>) {
    use objc2_foundation::NSUserNotificationCenter;

    let center = NSUserNotificationCenter::defaultUserNotificationCenter();
    for delivered in center.deliveredNotifications().iter() {
        let title = delivered.title().map(|s| s.to_string()).unwrap_or_default();
        let body = delivered
            .informativeText()
            .map(|s| s.to_string())
            .unwrap_or_default();
        if let Some(i) = entries.iter().position(|w| w.title == title && w.body == body) {
            entries.swap_remove(i);
            center.removeDeliveredNotification(&delivered);
            if entries.is_empty() {
                break;
            }
        }
    }
}

/// Show a native notification whose click (body or "Open" button) raises the
/// main window and then runs `on_click`. Returns immediately; the blocking
/// wait for the user's click runs on a dedicated OS thread. `label` only names
/// the notification in the failure log.
pub(crate) fn show_clickable(
    app: AppHandle,
    title: String,
    body: String,
    label: &'static str,
    on_click: impl FnOnce(&AppHandle) + Send + 'static,
) {
    show_clickable_opts(app, title, body, label, true, on_click)
}

/// `show_clickable`; `raise_main = false` leaves window raising to `on_click`
/// (a Focus reminder raises the Focus window instead).
pub(crate) fn show_clickable_opts(
    app: AppHandle,
    title: String,
    body: String,
    label: &'static str,
    raise_main: bool,
    on_click: impl FnOnce(&AppHandle) + Send + 'static,
) {
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

    let waiting_id = register_waiting(&app, &title, &body);

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
                        if let Some(win) = app.get_webview_window("main").filter(|_| raise_main) {
                            let _ = win.unminimize();
                            let _ = win.show();
                            let _ = win.set_focus();
                        }
                        on_click(&app);
                    }
                });
            }
            Err(err) => {
                tracing::warn!("failed to show {label}: {err}");
            }
        }
        unregister_waiting(waiting_id);
    });
}

/// Show a native notification for a pending permission/question on a run the
/// user isn't currently viewing. A click emits `NOTIFICATION_CLICKED_EVENT`
/// with the originating project/run.
#[tauri::command]
pub async fn show_permission_notification(
    app: AppHandle,
    title: String,
    body: String,
    project_id: String,
    run_id: String,
) -> Result<(), String> {
    show_clickable(app, title, body, "permission notification", move |app| {
        let _ = app.emit(
            NOTIFICATION_CLICKED_EVENT,
            NotificationClick { project_id, run_id },
        );
    });
    Ok(())
}

/// Show a native notification that a run finished while the app was in the
/// background. A click emits `NOTIFICATION_CLICKED_EVENT` with the finished
/// project/run, so the frontend opens it exactly like a permission click.
#[tauri::command]
pub async fn show_run_finished_notification(
    app: AppHandle,
    title: String,
    body: String,
    project_id: String,
    run_id: String,
) -> Result<(), String> {
    show_clickable(app, title, body, "run finished notification", move |app| {
        let _ = app.emit(
            NOTIFICATION_CLICKED_EVENT,
            NotificationClick { project_id, run_id },
        );
    });
    Ok(())
}

/// Show a native calendar-reminder notification for an upcoming event. A click
/// emits `CALENDAR_REMINDER_CLICKED_EVENT` with the event id so the frontend can
/// open that event's detail drawer.
#[tauri::command]
pub async fn show_calendar_reminder(
    app: AppHandle,
    title: String,
    body: String,
    event_id: String,
) -> Result<(), String> {
    show_clickable(app, title, body, "calendar reminder", move |app| {
        let _ = app.emit(CALENDAR_REMINDER_CLICKED_EVENT, ReminderClick { event_id });
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(id: u64) -> Waiting {
        Waiting {
            id,
            title: format!("t{id}"),
            body: String::new(),
        }
    }

    #[test]
    fn push_capped_withdraws_oldest_past_cap() {
        let mut queue = std::collections::VecDeque::new();
        for id in 0..3 {
            assert!(push_capped(&mut queue, w(id), 3).is_empty());
        }
        assert_eq!(push_capped(&mut queue, w(3), 3), vec![w(0)]);
        // Still queued until its waiter ends, so a missed withdraw is retried.
        assert_eq!(push_capped(&mut queue, w(4), 3), vec![w(0), w(1)]);
        unregister_from(&mut queue, 0);
        assert_eq!(queue.iter().map(|w| w.id).collect::<Vec<_>>(), [1, 2, 3, 4]);
    }
}
