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
pub const CONDUCTOR_SYSTEM_PROMPT: &str = r#"You are a normal coding assistant session that can ALSO delegate work to
WORKER sessions when that genuinely helps.

Default: do the work yourself, directly. Answer questions, make small or
single-step changes, investigate, read and edit code, run commands — exactly as
you would without workers. Do NOT spawn a worker for a quick task, a question,
or anything you can finish in a few steps; delegating that only adds latency
and cost.

Spawn workers only when it clearly pays off:
- the task is large and splits into independent parts that can run in parallel;
- you need a separate role with its own fresh context, e.g. an independent
  reviewer/QA checking work it did not write;
- a long-running side task should proceed while you keep working with the human.
When you do delegate, break the work into clear roles, spawn the workers, read
their results, and iterate until the goal is met — asking the human only at
meaningful checkpoints. You may keep doing direct work yourself meanwhile.

You control workers with these MCP tools (server `conductor`):
- session_spawn(role_label, prompt, instruction?, engine?, model?, permission_mode?)
    -> spawns a worker and returns { worker_id }. `instruction` becomes the
       worker's role/system framing; `prompt` is its first task message.
- session_list() -> all your workers and their status.
- session_poll(worker_ids?) -> instant status of workers (running | idle | ...).
- session_wait(worker_ids, timeout_ms?) -> block until those workers finish their
    current turn (status != running) or the timeout elapses. The result carries
    `waited_human` (a ready-to-quote string like "1m 3s") and `waited_ms` — the
    time THIS call actually blocked. When you tell the human how long you waited,
    quote `waited_human` verbatim. NEVER report `timeout_ms`: it is only the upper
    bound you asked for, not the elapsed time (quoting it is how a 1-minute wait
    gets misreported as "10 minutes"). This call streams progress and stays alive
    for the whole wait — it will not time out on you; trust the values it returns.
    Use session_poll any time you just want a quick status without blocking.
- session_read(worker_id) -> the worker's latest assistant reply (its result).
- session_send(worker_id, text) -> give a worker a follow-up turn.
- session_cancel(worker_id) -> stop a worker.

Permissions: a worker that needs approval for a tool is answered by the human via
the normal permission drawer (shown in this session). You do not handle
worker permissions yourself; if a worker is blocked waiting for approval it will
show as still running until the human responds. session_list / session_poll /
session_read / session_wait then add an `awaiting_permission` field (tools and
`waited_human`) to that worker: tell the human it needs their approval instead
of cancelling or replacing the worker.

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

Typical loop when you delegate:
1) Decide the roles you need (e.g. an implementer + a reviewer). Spawn them.
2) Either session_wait for them, or end your turn and wait for the auto-resume.
3) On resume (or after session_wait), session_poll/session_list to find the idle
   workers and session_read each finished worker's result.
4) Judge the results yourself (do NOT rely on string matching). Decide whether to
   send follow-ups, spawn a specialist, or conclude.
5) When the goal is met, write a concise final summary for the human.

Rules:
- Prefer doing it yourself; delegate only for the cases listed above.
- Respect any max-worker limit; never spin up workers without bound.
- Prefer running independent workers in parallel (spawn several, then wait once).
- If a worker stalls (session_wait times out) inspect it with session_read and
  decide: send guidance, cancel and respawn, or escalate to the human.
- Keep the human informed with short status lines between tool calls."#;

/// Default concurrent-worker cap for a conductor that was never given one
/// (fresh start without `max_workers`, or a legacy session auto-upgraded).
pub const DEFAULT_MAX_WORKERS: usize = 10;

/// Claude permission-rule pattern matching every tool of the built-in
/// `conductor` MCP server. Auto-allowed for conductor runs that are NOT in
/// `bypassPermissions`, so orchestration calls (session_poll, session_wait…)
/// never raise a permission modal while file work stays gated.
pub const CONDUCTOR_TOOLS_ALLOW: &str = "mcp__conductor";

/// The full system framing a conductor run gets: the orchestration prompt plus
/// its worker cap. Injected on first launch AND on every resume (neither the
/// Claude SDK nor Codex persist an appended system prompt across a resume).
pub fn conductor_framing(max_workers: usize) -> String {
    format!(
        "{}\n\nYour worker cap for this session is {} concurrent workers.",
        CONDUCTOR_SYSTEM_PROMPT, max_workers
    )
}

/// The extra system framing a run launches/resumes with: its persisted role
/// instruction (workers) and/or the session framing (top-level runs), joined.
/// `None` when the run has neither.
pub fn run_framing(instruction: Option<&str>, launch: Option<&ConductorLaunch>) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(i) = instruction.map(str::trim).filter(|i| !i.is_empty()) {
        parts.push(i.to_string());
    }
    if let Some(c) = launch {
        parts.push(conductor_framing(c.max_workers));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

/// One-time framing seed for a resumed Codex thread that was never started with
/// it. Codex ignores `developerInstructions` (and `config.developer_instructions`)
/// on `thread/resume`, so the framing is prepended to that turn's prompt instead;
/// once it is in the thread history it persists, so this is sent only while
/// `runs.framing_seeded` is 0. Returns the block to prepend, or `None`.
pub fn codex_resume_seed(framing: Option<&str>, already_seeded: bool) -> Option<String> {
    if already_seeded {
        return None;
    }
    let framing = framing.map(str::trim).filter(|f| !f.is_empty())?;
    Some(format!(
        "<session-framing>\nThe following are standing instructions for this whole session \
(they apply to this and every later turn; do not reply to them directly):\n\n{}\n</session-framing>",
        framing
    ))
}

/// Remove a leading framing seed (see [`codex_resume_seed`]) from a user
/// message read back from a Codex rollout, so a reloaded transcript shows only
/// what the user typed. Text without a leading seed is returned unchanged.
pub fn strip_session_framing(text: &str) -> &str {
    const OPEN: &str = "<session-framing>";
    const CLOSE: &str = "</session-framing>";
    let t = text.trim_start();
    if !t.starts_with(OPEN) {
        return text;
    }
    match t.find(CLOSE) {
        Some(end) => t[end + CLOSE.len()..].trim_start(),
        None => text,
    }
}

/// What a launch/resume of a conductor run needs: the bearer token for the
/// loopback `conductor` MCP server and the cap baked into its framing.
#[derive(Debug, Clone)]
pub struct ConductorLaunch {
    pub token: String,
    pub max_workers: usize,
}

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
    /// Conductor ids with an auto-wake nudge in flight. Set when a finished
    /// worker wakes its idle conductor, cleared when that conductor's turn ends
    /// (its `result`, or `run:done`). Serializes a burst of workers finishing together into a
    /// single resume — the woken conductor polls ALL its workers, covering the
    /// rest — and prevents a second resume while the first turn is still running.
    waking: HashSet<String>,
    /// Conductors whose sidecar is mid-turn (a `system/init` seen, its `result`
    /// not yet). A conductor can stay `running` in the DB long after its turn
    /// ended (a backgrounded Bash keeps the sidecar alive), so the DB status alone
    /// can't tell "busy" from "idle but alive".
    turn_active: HashSet<String>,
    /// Conductors a worker finished for while they were mid-turn. The wake is
    /// deferred, not dropped: it is delivered when that turn ends.
    pending_wake: HashSet<String>,
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
    /// Serializes "count running workers → start one" so parallel
    /// `session_spawn` / resuming `session_send` calls can't both pass the cap.
    start_lock: Arc<tokio::sync::Mutex<()>>,
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
                turn_active: HashSet::new(),
                pending_wake: HashSet::new(),
                wake_tx: None,
            })),
            start_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Hold while counting running workers and starting/resuming one, so the
    /// concurrent-worker cap check and the start are atomic.
    pub async fn lock_worker_start(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.start_lock.lock().await
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
    #[allow(dead_code)] // launches go through `reregister_session`; kept for tests
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

    /// Record a newly spawned worker under its conductor. `running` is how many
    /// of its workers are currently running — the cap limits concurrency only,
    /// so finished/cancelled workers don't count. Returns false when the
    /// conductor is unknown or already at its worker cap.
    pub fn add_worker(&self, conductor_run_id: &str, worker_id: &str, running: usize) -> bool {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(s) = g.sessions.get_mut(conductor_run_id) {
                if running >= s.max_workers {
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

    /// A tracked conductor's sidecar started a turn (`system/init`).
    pub fn turn_started(&self, conductor_run_id: &str) {
        if let Ok(mut g) = self.inner.lock() {
            if g.sessions.contains_key(conductor_run_id) {
                g.turn_active.insert(conductor_run_id.to_string());
            }
        }
    }

    /// A conductor's turn ended (its `result`, or its sidecar exited). Releases
    /// the auto-wake slot and returns whether a wake was deferred meanwhile, which
    /// the caller must now deliver.
    pub fn turn_ended(&self, conductor_run_id: &str) -> bool {
        if let Ok(mut g) = self.inner.lock() {
            g.turn_active.remove(conductor_run_id);
            g.waking.remove(conductor_run_id);
            g.pending_wake.remove(conductor_run_id)
        } else {
            false
        }
    }

    /// If the conductor is mid-turn, record a deferred wake and return true; the
    /// caller then leaves it alone. Checked under the same lock `turn_ended`
    /// takes, so a wake can't slip between "busy" and "turn just ended".
    pub fn defer_wake_if_busy(&self, conductor_run_id: &str) -> bool {
        if let Ok(mut g) = self.inner.lock() {
            if g.turn_active.contains(conductor_run_id) {
                g.pending_wake.insert(conductor_run_id.to_string());
                return true;
            }
        }
        false
    }

    /// Defer a wake unconditionally (the nudge couldn't be delivered now, e.g.
    /// the sidecar is shutting down); the conductor's next turn end delivers it.
    pub fn defer_wake(&self, conductor_run_id: &str) {
        if let Ok(mut g) = self.inner.lock() {
            g.pending_wake.insert(conductor_run_id.to_string());
        }
    }

    /// Claim the auto-wake slot for an idle conductor and mark it busy at once, so
    /// a second worker finishing before the nudge's `system/init` arrives defers
    /// instead of sending a duplicate nudge.
    pub fn begin_wake_turn(&self, conductor_run_id: &str) -> bool {
        if let Ok(mut g) = self.inner.lock() {
            if !g.waking.insert(conductor_run_id.to_string()) {
                return false;
            }
            g.turn_active.insert(conductor_run_id.to_string());
            true
        } else {
            false
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

    /// Forget a deleted run: drop its conductor session and EVERY token minted
    /// for it (so a still-running sidecar can no longer drive anything), its
    /// wake bookkeeping, and its id from any conductor's worker list.
    pub fn unregister_session(&self, run_id: &str) {
        if let Ok(mut g) = self.inner.lock() {
            g.sessions.remove(run_id);
            g.token_index.retain(|_, owner| owner != run_id);
            g.waking.remove(run_id);
            g.turn_active.remove(run_id);
            g.pending_wake.remove(run_id);
            for s in g.sessions.values_mut() {
                s.worker_ids.retain(|w| w != run_id);
            }
        }
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

/// Codex flavour of [`with_conductor`]: same loopback server, plus
/// `default_tools_approval_mode = "approve"` so the conductor's own `session_*`
/// calls never stop on an approval prompt (Codex otherwise asks per MCP call
/// under the `untrusted`/`on-request` policies). The sidecar maps both keys to
/// `-c mcp_servers.conductor.*` overrides. Kept out of the Claude map, whose MCP
/// config schema has no such key.
pub fn with_conductor_codex(
    mcp: serde_json::Value,
    base_url: &str,
    token: &str,
) -> serde_json::Value {
    let mut mcp = with_conductor(mcp, base_url, token);
    if let Some(cfg) = mcp.get_mut("conductor").and_then(|c| c.as_object_mut()) {
        // Only tag our own server — a user server named `conductor` wins above
        // and must keep its configured behavior.
        let ours = cfg.get("url").and_then(|u| u.as_str()) == Some(base_url);
        if ours {
            cfg.entry("default_tools_approval_mode".to_string())
                .or_insert_with(|| serde_json::Value::String("approve".to_string()));
        }
    }
    mcp
}

fn mint_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with(conductor: &str) -> ConductorState {
        let cst = ConductorState::new();
        cst.register_session(conductor, "project", 4);
        cst
    }

    #[test]
    fn wake_during_turn_is_deferred_then_delivered_at_turn_end() {
        let cst = state_with("c1");
        cst.turn_started("c1");
        assert!(cst.defer_wake_if_busy("c1"), "mid-turn conductor must defer");
        assert!(cst.turn_ended("c1"), "turn end must hand back the deferred wake");
        assert!(!cst.turn_ended("c1"), "a deferred wake is delivered once");
    }

    #[test]
    fn idle_alive_conductor_is_woken_once_per_turn() {
        let cst = state_with("c1");
        // Turn ended, sidecar still alive: not busy, so the wake goes through.
        assert!(!cst.defer_wake_if_busy("c1"));
        assert!(cst.begin_wake_turn("c1"));
        // A second worker finishing before the nudge's init defers instead of
        // sending a duplicate nudge.
        assert!(cst.defer_wake_if_busy("c1"));
        assert!(!cst.begin_wake_turn("c1"));
        // The nudged turn ends → slot released, deferred wake delivered.
        assert!(cst.turn_ended("c1"));
        assert!(cst.begin_wake_turn("c1"));
    }

    #[test]
    fn codex_conductor_server_auto_approves_and_carries_token() {
        let base = "http://127.0.0.1:4242/mcp";
        let mcp = with_conductor_codex(serde_json::json!({}), base, "tok");
        let c = &mcp["conductor"];
        assert_eq!(c["type"], "http");
        assert_eq!(c["url"], base);
        assert_eq!(c["headers"]["Authorization"], "Bearer tok");
        assert_eq!(c["default_tools_approval_mode"], "approve");
    }

    #[test]
    fn codex_conductor_keeps_user_server_named_conductor_untouched() {
        let user = serde_json::json!({ "conductor": { "type": "stdio", "command": "x" } });
        let mcp = with_conductor_codex(user, "http://127.0.0.1:1/mcp", "tok");
        assert_eq!(mcp["conductor"]["command"], "x");
        assert!(mcp["conductor"].get("default_tools_approval_mode").is_none());
    }

    #[test]
    fn framing_includes_prompt_and_cap() {
        let f = conductor_framing(7);
        assert!(f.starts_with(CONDUCTOR_SYSTEM_PROMPT));
        assert!(f.contains("7 concurrent workers"));
    }

    #[test]
    fn framing_is_hybrid_not_delegate_only() {
        let p = CONDUCTOR_SYSTEM_PROMPT;
        // Every session is a conductor: it must work directly by default…
        assert!(p.contains("do the work yourself, directly"));
        assert!(!p.contains("You do not do the implementation work yourself"));
        assert!(!p.contains("CONDUCTOR"));
        // …while keeping the full control-plane guidance.
        for needle in [
            "session_spawn(",
            "session_wait(",
            "session_poll(",
            "session_read(",
            "session_send(",
            "session_cancel(",
            "waited_human",
            "awaiting_permission",
            "AUTOMATICALLY RESUMED",
            "max-worker limit",
        ] {
            assert!(p.contains(needle), "framing lost: {needle}");
        }
    }

    #[test]
    fn run_framing_joins_instruction_and_session_framing() {
        let launch = ConductorLaunch { token: "t".into(), max_workers: 3 };
        assert_eq!(run_framing(None, None), None);
        assert_eq!(run_framing(Some("  "), None), None);
        assert_eq!(run_framing(Some("You are a reviewer."), None).as_deref(), Some("You are a reviewer."));
        let both = run_framing(Some("Role X"), Some(&launch)).unwrap();
        assert!(both.starts_with("Role X\n\n"));
        assert!(both.ends_with(&conductor_framing(3)));
    }

    #[test]
    fn codex_resume_seed_only_when_not_yet_seeded() {
        assert_eq!(codex_resume_seed(Some("F"), true), None);
        assert_eq!(codex_resume_seed(None, false), None);
        assert_eq!(codex_resume_seed(Some(" "), false), None);
        let seed = codex_resume_seed(Some("FRAMING-XYZ"), false).unwrap();
        assert!(seed.starts_with("<session-framing>\n"));
        assert!(seed.ends_with("\nFRAMING-XYZ\n</session-framing>"));
    }

    #[test]
    fn strip_session_framing_removes_only_a_leading_seed() {
        let seed = codex_resume_seed(Some("FRAMING"), false).unwrap();
        assert_eq!(strip_session_framing(&format!("{seed}\n\nhello")), "hello");
        assert_eq!(strip_session_framing(&seed), "");
        assert_eq!(strip_session_framing("plain text"), "plain text");
        // Not leading → user content, kept verbatim.
        let mid = format!("see this: {seed}");
        assert_eq!(strip_session_framing(&mid), mid);
        // Unterminated → kept verbatim rather than swallowing the message.
        assert_eq!(strip_session_framing("<session-framing> oops"), "<session-framing> oops");
    }

    #[test]
    fn unregister_revokes_every_token_and_worker_links() {
        let cst = ConductorState::new();
        let t1 = cst.register_session("c1", "p", 4);
        // A restart-style re-registration mints a second token for the same run.
        let t2 = cst.reregister_session("c1", "p", 4, vec!["w1".into()]);
        let other = cst.register_session("c2", "p", 4);
        assert!(cst.add_worker("c2", "c1-orphan", 0));
        cst.turn_started("c1");
        cst.unregister_session("c1");
        assert!(cst.session("c1").is_none());
        assert!(cst.resolve_token(&t1).is_none() && cst.resolve_token(&t2).is_none());
        assert!(!cst.defer_wake_if_busy("c1"));
        assert_eq!(cst.resolve_token(&other).as_deref(), Some("c2"));
        // Deleting a worker detaches it from its conductor.
        cst.unregister_session("c1-orphan");
        assert!(!cst.owns_worker("c2", "c1-orphan"));
    }

    #[test]
    fn worker_cap_counts_only_running_workers() {
        let cst = ConductorState::new();
        cst.register_session("c", "p", 2);
        assert!(cst.add_worker("c", "w1", 0));
        assert!(cst.add_worker("c", "w2", 1));
        assert!(!cst.add_worker("c", "w3", 2));
        // w1/w2 finished → 0 running: a third (and fourth) spawn fits.
        assert!(cst.add_worker("c", "w3", 0));
        assert!(cst.add_worker("c", "w4", 1));
        assert!(cst.owns_worker("c", "w1") && cst.owns_worker("c", "w4"));
    }

    #[test]
    fn untracked_runs_never_become_busy() {
        let cst = ConductorState::new();
        cst.turn_started("plain-run");
        assert!(!cst.defer_wake_if_busy("plain-run"));
    }
}
