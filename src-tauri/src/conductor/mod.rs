//! Conductor: AI-driven orchestration of multiple worker sessions.
//!
//! A **conductor run** (itself an ordinary Claude session) orchestrates N
//! **worker runs** by
//! calling `session_*` MCP tools. Those tools are served by a tiny in-process
//! Rust MCP server (Streamable HTTP, bound to 127.0.0.1 only) that has direct
//! access to the app's `RunRegistry` + `Db`, so it can spawn/read/drive workers
//! by calling the same `start_run_inner` / `send_user_message_inner` /
//! `cancel_run_inner` the desktop UI and Remote Control already use.
//!
//! Security: each conductor session gets a random bearer token bound to its run
//! id. The MCP server only accepts requests carrying a known token and only lets
//! that conductor touch workers it spawned. The listener binds to loopback only.

pub mod commands;
pub mod mcp_http;
pub mod tools;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// System prompt appended to a conductor run, teaching it the control-plane
/// tools and the loop to run. Kept deliberately short; the orchestration logic
/// lives in the model, not in a hardcoded FE state machine.
pub const CONDUCTOR_SYSTEM_PROMPT: &str = r#"You are the CONDUCTOR of a multi-agent session.

You do not do the implementation work yourself. Instead you break the goal into
roles, spawn WORKER sessions to do the work, read their results, and iterate
until the goal is met — asking the human only at meaningful checkpoints.

You control workers with these MCP tools (server `conductor`):
- session_spawn(role_label, prompt, instruction?, engine?, model?, permission_mode?)
    -> spawns a worker and returns { worker_id }. `instruction` becomes the
       worker's role/system framing; `prompt` is its first task message.
- session_list() -> all your workers and their status.
- session_poll(worker_ids?) -> instant status of workers (running | idle | ...).
- session_wait(worker_ids, timeout_ms?) -> block until those workers finish their
    current turn (status != running) or the timeout elapses. Returns `waited_ms`,
    the time this call actually blocked. When you tell the human how long you
    waited, use `waited_ms` — never `timeout_ms`, which is only the upper bound.
- session_read(worker_id) -> the worker's latest assistant reply (its result).
- session_send(worker_id, text) -> give a worker a follow-up turn.
- session_cancel(worker_id) -> stop a worker.

Permissions: a worker that needs approval for a tool is answered by the human via
the normal permission drawer (shown in the Conductor tab). You do not handle
worker permissions yourself; if a worker is blocked waiting for approval it will
show as still running until the human responds.

YOU ARE AUTOMATICALLY RESUMED WHEN A WORKER FINISHES. After spawning workers you
have two equally valid ways to stay in the loop:
  (a) Block in session_wait for them, then read + continue in the same turn; or
  (b) End your turn with a short status line to the human (e.g. "Spawned 2 workers
      — I'll report back when they finish"). The system resumes you as soon as a
      worker finishes its turn; you then poll/read and continue.
NEVER tell the human you will report back and then stop WITHOUT either blocking in
session_wait or relying on this auto-resume — a plain stop that does neither is a
broken promise. The auto-resume nudge covers every worker that finished, so when
you are resumed, session_poll / session_list ALL your workers (not just one).

Typical loop:
1) Decide the roles you need (e.g. an implementer + a reviewer). Spawn them.
2) Either session_wait for them, or end your turn and wait for the auto-resume.
3) On resume (or after session_wait), session_poll/session_list to find the idle
   workers and session_read each finished worker's result.
4) Judge the results yourself (do NOT rely on string matching). Decide whether to
   send follow-ups, spawn a specialist, or conclude.
5) When the goal is met, write a concise final summary for the human.

Rules:
- Respect any max-worker limit; never spin up workers without bound.
- Prefer running independent workers in parallel (spawn several, then wait once).
- If a worker stalls (session_wait times out) inspect it with session_read and
  decide: send guidance, cancel and respawn, or escalate to the human.
- Keep the human informed with short status lines between tool calls."#;

/// A live conductor session tracked in memory for the lifetime of the app run.
#[derive(Clone)]
pub struct ConductorSession {
    #[allow(dead_code)]
    pub conductor_run_id: String,
    pub project_id: String,
    #[allow(dead_code)]
    pub token: String,
    pub worker_ids: Vec<String>,
    pub max_workers: usize,
}

struct Inner {
    /// Port the loopback MCP server is bound to (0 until the server starts).
    port: u16,
    sessions: HashMap<String, ConductorSession>, // keyed by conductor_run_id
    token_index: HashMap<String, String>,        // token -> conductor_run_id
    /// Conductor ids with an auto-wake resume in flight. Set when a finished
    /// worker resumes its idle conductor, cleared when that conductor's turn ends
    /// (its `run:done`). Serializes a burst of workers finishing together into a
    /// single resume — the woken conductor polls ALL its workers, covering the
    /// rest — and prevents a second resume while the first turn is still running.
    waking: HashSet<String>,
    /// Sender for finished-run ids. The sidecar drain pushes every run's id here on
    /// `run:done`; a dedicated consumer task (started with the MCP server) performs
    /// the actual resume. This channel hop deliberately decouples the sidecar drain
    /// from `resume_run` — `resume_run` spawns a new drain, so a direct call would
    /// make the drain future's auto-traits cyclic and un-inferrable.
    wake_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
}

/// Managed Tauri state: the conductor registry + the MCP server's bound port.
#[derive(Clone)]
pub struct ConductorState {
    inner: Arc<Mutex<Inner>>,
}

impl Default for ConductorState {
    fn default() -> Self {
        Self::new()
    }
}

impl ConductorState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                port: 0,
                sessions: HashMap::new(),
                token_index: HashMap::new(),
                waking: HashSet::new(),
                wake_tx: None,
            })),
        }
    }

    pub fn set_port(&self, port: u16) {
        if let Ok(mut g) = self.inner.lock() {
            g.port = port;
        }
    }

    pub fn port(&self) -> u16 {
        self.inner.lock().map(|g| g.port).unwrap_or(0)
    }

    /// The URL a conductor run points its `conductor` MCP server at.
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.port())
    }

    /// Register a new conductor session and mint its bearer token.
    pub fn register_session(
        &self,
        conductor_run_id: &str,
        project_id: &str,
        max_workers: usize,
    ) -> String {
        let token = mint_token();
        if let Ok(mut g) = self.inner.lock() {
            g.token_index
                .insert(token.clone(), conductor_run_id.to_string());
            g.sessions.insert(
                conductor_run_id.to_string(),
                ConductorSession {
                    conductor_run_id: conductor_run_id.to_string(),
                    project_id: project_id.to_string(),
                    token: token.clone(),
                    worker_ids: Vec::new(),
                    max_workers,
                },
            );
        }
        token
    }

    /// Resolve a bearer token to its conductor run id (auth check).
    pub fn resolve_token(&self, token: &str) -> Option<String> {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.token_index.get(token).cloned())
    }

    pub fn session(&self, conductor_run_id: &str) -> Option<ConductorSession> {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.sessions.get(conductor_run_id).cloned())
    }

    /// Record a newly spawned worker under its conductor. Returns false when the
    /// conductor is unknown or already at its worker cap.
    pub fn add_worker(&self, conductor_run_id: &str, worker_id: &str) -> bool {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(s) = g.sessions.get_mut(conductor_run_id) {
                if s.worker_ids.len() >= s.max_workers {
                    return false;
                }
                s.worker_ids.push(worker_id.to_string());
                return true;
            }
        }
        false
    }

    /// Update a live conductor's worker cap. Returns false when the conductor is
    /// unknown (never registered, or the app restarted without it). The new cap
    /// takes effect on the next `session_spawn` — `add_worker` reads it live under
    /// the lock — so it can be raised or lowered while the conductor is running.
    pub fn set_max_workers(&self, conductor_run_id: &str, max_workers: usize) -> bool {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(s) = g.sessions.get_mut(conductor_run_id) {
                s.max_workers = max_workers;
                return true;
            }
        }
        false
    }

    /// Install the finished-run channel sender (called once when the MCP server's
    /// wake consumer task starts).
    pub fn set_wake_sender(&self, tx: tokio::sync::mpsc::UnboundedSender<String>) {
        if let Ok(mut g) = self.inner.lock() {
            g.wake_tx = Some(tx);
        }
    }

    /// Enqueue a finished run id for the wake consumer. A no-op before the consumer
    /// is installed. Never blocks and never calls `resume_run` itself — that keeps
    /// the sidecar drain's future free of a cyclic dependency on itself.
    pub fn notify_run_finished(&self, run_id: String) {
        if let Ok(g) = self.inner.lock() {
            if let Some(tx) = g.wake_tx.as_ref() {
                let _ = tx.send(run_id);
            }
        }
    }

    /// Claim the single in-flight auto-wake slot for a conductor. Returns true to
    /// the first caller (which should perform the resume) and false while a wake is
    /// already pending — so a burst of workers finishing produces exactly one
    /// resume. Cleared by [`finish_wake`] when the conductor's turn ends.
    pub fn begin_wake(&self, conductor_run_id: &str) -> bool {
        if let Ok(mut g) = self.inner.lock() {
            g.waking.insert(conductor_run_id.to_string())
        } else {
            false
        }
    }

    /// Release the auto-wake slot (the conductor's turn has ended, so the next
    /// worker completion may wake it again). No-op if no wake was pending.
    pub fn finish_wake(&self, conductor_run_id: &str) {
        if let Ok(mut g) = self.inner.lock() {
            g.waking.remove(conductor_run_id);
        }
    }

    /// Whether `worker_id` belongs to `conductor_run_id` (scoping check).
    pub fn owns_worker(&self, conductor_run_id: &str, worker_id: &str) -> bool {
        self.session(conductor_run_id)
            .map(|s| s.worker_ids.iter().any(|w| w == worker_id))
            .unwrap_or(false)
    }

    /// Restore a conductor session on app startup with its known workers. Mints a
    /// fresh token (the old one died with the previous process). Used to
    /// re-establish worker ownership so a resumed conductor keeps control.
    pub fn reregister_session(
        &self,
        conductor_run_id: &str,
        project_id: &str,
        max_workers: usize,
        worker_ids: Vec<String>,
    ) -> String {
        let token = mint_token();
        if let Ok(mut g) = self.inner.lock() {
            g.token_index
                .insert(token.clone(), conductor_run_id.to_string());
            g.sessions.insert(
                conductor_run_id.to_string(),
                ConductorSession {
                    conductor_run_id: conductor_run_id.to_string(),
                    project_id: project_id.to_string(),
                    token: token.clone(),
                    worker_ids,
                    max_workers,
                },
            );
        }
        token
    }

    /// Look up the current bearer token for a conductor (needed to re-inject the
    /// `conductor` MCP server when a conductor run is resumed).
    pub fn token_for(&self, conductor_run_id: &str) -> Option<String> {
        self.session(conductor_run_id).map(|s| s.token)
    }
}

/// Add the built-in `conductor` MCP server (Streamable HTTP, loopback) to an
/// already-resolved MCP map. Only injected for conductor runs, which carry a
/// `conductor_token`. A user server literally named `conductor` takes precedence.
pub fn with_conductor(
    mcp: serde_json::Value,
    base_url: &str,
    token: &str,
) -> serde_json::Value {
    let mut map = match mcp {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    map.entry("conductor".to_string()).or_insert_with(|| {
        serde_json::json!({
            "type": "http",
            "url": base_url,
            "headers": { "Authorization": format!("Bearer {}", token) },
        })
    });
    serde_json::Value::Object(map)
}

fn mint_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}
