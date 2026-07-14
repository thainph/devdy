pub mod broker;
pub mod permission;
pub mod pricing;
pub mod session_watcher;
pub mod sidecar;

use std::collections::HashMap;
use std::sync::Arc;
use tokio::process::{Child, ChildStdin};
use tokio::sync::{oneshot, Mutex};

pub struct RunHandles {
    pub child: Child,
    /// Live stdin writer for stream-json (Claude) runs. None for one-shot codex runs.
    pub stdin: Option<ChildStdin>,
    /// Claude session id captured from the first `system.init` event. Held here
    /// so the drain task can update it in place; `resume_run` reads from the DB.
    #[allow(dead_code)]
    pub session_id: Arc<Mutex<Option<String>>>,
    /// Live log buffer shared with the stdout-drain task and `send_user_message`,
    /// so user follow-up messages are persisted to the log file alongside the
    /// stream from the subprocess.
    pub log_buf: Arc<Mutex<String>>,
    /// GĐ3: per-run credential broker handle (Claude runs only; None for codex).
    /// Held here so it lives exactly as long as the run — when the registry drops
    /// the `RunHandles`, `BrokerHandle::drop` aborts the accept loop and removes
    /// the socket (mirrors `kill_on_drop`). No explicit teardown is needed.
    // Set once the broker is wired into the run spawn path (GĐ3); held-for-drop.
    #[allow(dead_code)]
    pub broker: Option<broker::BrokerHandle>,
}

pub type RunRegistry = Arc<Mutex<HashMap<String, RunHandles>>>;

pub fn new_registry() -> RunRegistry {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Pending broker `Ask` approvals, keyed by a fresh `request_id`. A `ModalApprover`
/// registers a oneshot sender here before emitting the permission event; the
/// unchanged `respond_permission` command resolves it when the user answers.
///
/// One global map is enough because keys are UUIDs (globally unique across runs).
/// Orphaned entries (run ended before the user answered) are harmless: the sender
/// drops with the `ModalApprover`, so the awaiting `rx` gets `Err` → fail-closed
/// deny. The map never holds a token.
pub type BrokerApprovals = Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>;

pub fn new_broker_approvals() -> BrokerApprovals {
    Arc::new(Mutex::new(HashMap::new()))
}
