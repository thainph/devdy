pub mod permission;
pub mod pricing;
pub mod session_watcher;
pub mod sidecar;

use std::collections::HashMap;
use std::sync::Arc;
use tokio::process::{Child, ChildStdin};
use tokio::sync::Mutex;

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
}

pub type RunRegistry = Arc<Mutex<HashMap<String, RunHandles>>>;

pub fn new_registry() -> RunRegistry {
    Arc::new(Mutex::new(HashMap::new()))
}
