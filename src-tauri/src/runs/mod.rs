pub mod broker;
pub mod permission;
pub mod pricing;
pub mod session_watcher;
pub mod sidecar;
pub mod ssh_access;

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
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
    /// GĐ7: registration of this run in the app-wide singleton broker (Claude
    /// runs only; None for codex). The broker outlives every run; this guard only
    /// records that the run is alive — so its `Ask` modal can be routed — and, on
    /// Drop, removes that record from `BrokerRuns`. Held here so it lives exactly
    /// as long as the run: when the registry drops the `RunHandles`, the run is
    /// automatically deregistered. No socket is created or torn down per run.
    #[allow(dead_code)]
    pub broker_run: Option<BrokerRunGuard>,
}

pub type RunRegistry = Arc<Mutex<HashMap<String, RunHandles>>>;

pub fn new_registry() -> RunRegistry {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Per-run context the singleton broker needs to serve a request for that run.
/// Currently just the working directory (surfaced as the `Ask` modal's `cwd`);
/// never holds a token. `Clone` so the resolver can copy it out under a short
/// lock without holding the map locked across the await.
#[derive(Clone, Default)]
pub struct BrokerRunCtx {
    pub cwd: Option<String>,
}

/// Live runs known to the singleton broker, keyed by `run_id`. Presence in this
/// map == "this run is alive, its `Ask` modal can be shown". Absence == the run
/// ended → the broker fail-closed denies any `Ask` for it (a read/allow needs no
/// approver and still works). A plain `std::sync::Mutex` (not tokio) so
/// `BrokerRunGuard::drop` can deregister synchronously from any context.
pub type BrokerRuns = Arc<StdMutex<HashMap<String, BrokerRunCtx>>>;

pub fn new_broker_runs() -> BrokerRuns {
    Arc::new(StdMutex::new(HashMap::new()))
}

/// RAII registration of one run in `BrokerRuns`. Constructing it inserts the
/// run's context; dropping it removes the run. Stored in `RunHandles` so the
/// existing registry teardown (cancel_run / drain end) deregisters the run for
/// free — no extra cleanup call sites.
pub struct BrokerRunGuard {
    runs: BrokerRuns,
    run_id: String,
}

impl BrokerRunGuard {
    /// Register `run_id` (with its context) as alive and return the guard.
    pub fn register(runs: BrokerRuns, run_id: String, ctx: BrokerRunCtx) -> Self {
        if let Ok(mut map) = runs.lock() {
            map.insert(run_id.clone(), ctx);
        }
        Self { runs, run_id }
    }
}

impl Drop for BrokerRunGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = self.runs.lock() {
            map.remove(&self.run_id);
        }
    }
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
