//! Permission request event emitted to the frontend, plus the backend-side
//! record of requests still awaiting an answer.
//!
//! Permission interception is handled by the Agent-SDK sidecar's `canUseTool`
//! callback (see `runs/sidecar.rs`); the drain emits the payload below on
//! `run:permission_request:<run_id>`. That event is fire-and-forget: if no
//! webview listener is attached at that instant (e.g. the live session was
//! evicted), the request is lost and the sidecar waits forever. The pending
//! store lets a (re)attached listener recover still-open requests and lets the
//! conductor see which worker is blocked on an approval.

use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Serialize, Clone)]
pub struct PermissionRequestEvent {
    pub run_id: String,
    pub request_id: String,
    pub tool_name: String,
    pub tool_input: Value,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    /// Bridge-rendered prompt sentence (e.g. "Claude wants to read foo.txt").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Bridge-rendered subtitle describing the effect of the tool call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Short noun phrase for the action (e.g. "Read file"), for compact UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// A permission request the sidecar is still blocked on.
#[derive(Debug, Clone)]
pub struct PendingPermission {
    pub event: PermissionRequestEvent,
    pub requested_at: chrono::DateTime<chrono::Utc>,
}

/// run_id -> open requests, in arrival order.
fn pending_store() -> &'static Mutex<HashMap<String, Vec<PendingPermission>>> {
    static PENDING: OnceLock<Mutex<HashMap<String, Vec<PendingPermission>>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record a request the drain just emitted.
pub fn track_pending(event: PermissionRequestEvent) {
    let mut map = pending_store().lock().unwrap_or_else(|e| e.into_inner());
    map.entry(event.run_id.clone()).or_default().push(PendingPermission {
        event,
        requested_at: chrono::Utc::now(),
    });
}

/// Forget a request once its answer has been delivered to the sidecar.
pub fn resolve_pending(run_id: &str, request_id: &str) {
    let mut map = pending_store().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(list) = map.get_mut(run_id) {
        list.retain(|p| p.event.request_id != request_id);
        if list.is_empty() {
            map.remove(run_id);
        }
    }
}

/// Drop every open request of a run whose sidecar has exited.
pub fn clear_pending(run_id: &str) {
    let mut map = pending_store().lock().unwrap_or_else(|e| e.into_inner());
    map.remove(run_id);
}

/// Snapshot of the requests a run is still waiting on (oldest first).
pub fn pending_for(run_id: &str) -> Vec<PendingPermission> {
    let map = pending_store().lock().unwrap_or_else(|e| e.into_inner());
    map.get(run_id).cloned().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evt(run_id: &str, request_id: &str) -> PermissionRequestEvent {
        PermissionRequestEvent {
            run_id: run_id.to_string(),
            request_id: request_id.to_string(),
            tool_name: "Bash".to_string(),
            tool_input: Value::Null,
            session_id: None,
            cwd: None,
            title: None,
            description: None,
            display_name: None,
        }
    }

    #[test]
    fn tracks_resolves_and_clears() {
        let run = "test-run-pending-store";
        track_pending(evt(run, "a"));
        track_pending(evt(run, "b"));
        let ids: Vec<String> = pending_for(run).into_iter().map(|p| p.event.request_id).collect();
        assert_eq!(ids, vec!["a", "b"]);

        resolve_pending(run, "a");
        let ids: Vec<String> = pending_for(run).into_iter().map(|p| p.event.request_id).collect();
        assert_eq!(ids, vec!["b"]);

        clear_pending(run);
        assert!(pending_for(run).is_empty());
    }
}
