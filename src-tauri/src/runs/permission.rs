//! Permission request event emitted to the frontend.
//!
//! Permission interception is handled by the Agent-SDK sidecar's `canUseTool`
//! callback (see `runs/sidecar.rs`); this module only defines the event payload
//! the drain emits on `run:permission_request:<run_id>`.

use serde::Serialize;
use serde_json::Value;

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
