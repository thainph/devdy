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
    /// A broker prompt (gh / glab / ssh / aws / az / terraform): it can only be
    /// answered once — there is no standing Always for a credential use.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub once_only: bool,
}

/// Built-in `devdy` MCP tools that never need a prompt: the user's own todos,
/// notes and saved Slack threads (Devdy-local data, nothing is posted to Slack),
/// plus read-only lookups (accounts, skills, rules, projects, sessions, git).
/// Anything that runs commands remotely (`vps_run`) or changes skills, rules,
/// projects or repos still asks.
const AUTO_ALLOWED_DEVDY_TOOLS: &[&str] = &[
    "todos_add",
    "todos_clear_done",
    "todos_delete",
    "todos_done",
    "todos_list",
    "todos_reorder",
    "todos_set_due",
    "todos_set_project",
    "todos_update",
    "notes_append",
    "notes_create",
    "notes_delete",
    "notes_list",
    "notes_read",
    "notes_reorder",
    "notes_search",
    "notes_set_project",
    "notes_update",
    "slack_threads_delete",
    "slack_threads_list",
    "slack_threads_read",
    "slack_threads_rename",
    "slack_threads_search",
    "slack_threads_set_project",
    "git_accounts_list",
    "cloud_accounts_list",
    "vps_list",
    "skills_list",
    "skills_read",
    "skills_list_files",
    "skills_read_file",
    "rules_list",
    "rules_read",
    "project_info",
    "projects_list",
    "repos_list",
    "sessions_recent",
    "sessions_search",
    "session_read",
    "file_tree",
    "git_status",
    "git_diff",
];

/// Whether a permission prompt for `tool_name` is answered "allow" without
/// asking (both engines name these `mcp__devdy__<tool>`).
pub fn is_auto_allowed(tool_name: &str) -> bool {
    tool_name
        .strip_prefix("mcp__devdy__")
        .is_some_and(|t| AUTO_ALLOWED_DEVDY_TOOLS.contains(&t))
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
/// Returns whether it was still pending.
pub fn resolve_pending(run_id: &str, request_id: &str) -> bool {
    let mut map = pending_store().lock().unwrap_or_else(|e| e.into_inner());
    let Some(list) = map.get_mut(run_id) else {
        return false;
    };
    let before = list.len();
    list.retain(|p| p.event.request_id != request_id);
    let removed = list.len() != before;
    if list.is_empty() {
        map.remove(run_id);
    }
    removed
}

/// Claim a request for answering: remove and return it, or None when it was
/// already answered (by the desktop, a phone, a standing choice) or withdrawn.
/// Exactly one answer wins; a loser must not reach the sidecar.
pub fn take_pending(run_id: &str, request_id: &str) -> Option<PendingPermission> {
    let mut map = pending_store().lock().unwrap_or_else(|e| e.into_inner());
    let list = map.get_mut(run_id)?;
    let i = list.iter().position(|p| p.event.request_id == request_id)?;
    let taken = list.remove(i);
    if list.is_empty() {
        map.remove(run_id);
    }
    Some(taken)
}

/// Put back a request whose answer could not be delivered, so it can be retried.
pub fn restore_pending(p: PendingPermission) {
    let mut map = pending_store().lock().unwrap_or_else(|e| e.into_inner());
    let list = map.entry(p.event.run_id.clone()).or_default();
    if !list.iter().any(|x| x.event.request_id == p.event.request_id) {
        list.push(p);
        list.sort_by_key(|x| x.requested_at);
    }
}

/// A still-open request, without claiming it.
pub fn peek_pending(run_id: &str, request_id: &str) -> Option<PendingPermission> {
    let map = pending_store().lock().unwrap_or_else(|e| e.into_inner());
    map.get(run_id)?
        .iter()
        .find(|p| p.event.request_id == request_id)
        .cloned()
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

/// Snapshot of every open request across all runs (oldest first per run).
pub fn all_pending() -> Vec<PendingPermission> {
    let map = pending_store().lock().unwrap_or_else(|e| e.into_inner());
    map.values().flatten().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_allows_only_safe_devdy_tools() {
        assert!(is_auto_allowed("mcp__devdy__todos_add"));
        assert!(is_auto_allowed("mcp__devdy__notes_delete"));
        assert!(is_auto_allowed("mcp__devdy__slack_threads_read"));
        assert!(is_auto_allowed("mcp__devdy__git_accounts_list"));
        assert!(is_auto_allowed("mcp__devdy__skills_read"));
        assert!(is_auto_allowed("mcp__devdy__rules_read"));
        assert!(!is_auto_allowed("mcp__devdy__vps_run"));
        assert!(!is_auto_allowed("mcp__devdy__skills_update"));
        assert!(!is_auto_allowed("mcp__devdy__rules_delete"));
        assert!(!is_auto_allowed("mcp__devdy__projects_create"));
        assert!(!is_auto_allowed("mcp__other__todos_add"));
        assert!(!is_auto_allowed("todos_add"));
        assert!(!is_auto_allowed("Bash"));
    }

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
            once_only: false,
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

    #[test]
    fn only_one_claim_wins_and_a_failed_delivery_can_be_retried() {
        let run = "test-run-claim";
        track_pending(evt(run, "a"));
        let first = take_pending(run, "a").expect("first claim wins");
        assert!(take_pending(run, "a").is_none(), "a second answer must lose");
        assert!(peek_pending(run, "a").is_none());
        restore_pending(first);
        assert!(peek_pending(run, "a").is_some(), "restored for a retry");
        assert!(take_pending(run, "a").is_some());
        assert!(take_pending(run, "missing").is_none());
        clear_pending(run);
    }

    #[test]
    fn all_pending_spans_every_run() {
        let (r1, r2) = ("test-run-all-pending-1", "test-run-all-pending-2");
        track_pending(evt(r1, "x1"));
        track_pending(evt(r2, "x2"));
        let ids: Vec<String> = all_pending()
            .into_iter()
            .filter(|p| p.event.run_id == r1 || p.event.run_id == r2)
            .map(|p| p.event.request_id)
            .collect();
        assert!(ids.contains(&"x1".to_string()) && ids.contains(&"x2".to_string()));
        clear_pending(r1);
        clear_pending(r2);
    }
}
