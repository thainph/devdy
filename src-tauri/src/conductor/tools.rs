//! Implementation of the `session_*` control tools. Each tool reaches into the
//! app's managed state (`Db`, `RunRegistry`) via the `AppHandle` and reuses the
//! same core the desktop UI / Remote Control use (`start_run_inner`,
//! `send_user_message_inner`, `cancel_run_inner`, `resume_run`).

use serde_json::{json, Value};
use sqlx::Row;
use std::path::Path;
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager};

use super::mcp_http::HttpState;
use crate::commands::runs::{start_run_inner, StartRunPayload};
use crate::db::Db;
use crate::runs::RunRegistry;

const DEFAULT_WAIT_MS: u64 = 120_000;
const MAX_WAIT_MS: u64 = 600_000;
pub(crate) const POLL_INTERVAL_MS: u64 = 400;
/// How long a permission request must stay unanswered before a worker is
/// reported as `awaiting_permission`.
const AWAITING_PERMISSION_MIN_MS: u64 = 10_000;

/// Format an elapsed duration in ms as a short, human-readable string the model
/// can quote verbatim (e.g. "58s", "1m 3s", "10m 0s"). Giving the model a ready
/// string stops it inventing a wait time from the `timeout_ms` ceiling it passed
/// in — the single most common "waited 10 minutes" misreport.
pub(crate) fn format_waited(ms: u64) -> String {
    let total_secs = ms / 1000;
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    if mins == 0 {
        format!("{secs}s")
    } else {
        format!("{mins}m {secs}s")
    }
}

/// The approvals a worker's sidecar is blocked on, oldest first, or `None` when
/// it isn't waiting on any. A worker stuck here makes no progress until a human
/// answers in the desktop permission drawer, so the conductor should surface it
/// to the user rather than treat the worker as merely slow.
pub(crate) fn awaiting_permission(worker_id: &str) -> Option<Value> {
    let pending = crate::runs::permission::pending_for(worker_id);
    let oldest = pending.first()?;
    let waited_ms = chrono::Utc::now()
        .signed_duration_since(oldest.requested_at)
        .num_milliseconds()
        .max(0) as u64;
    // Standing allow-list answers land within milliseconds; only a request that
    // has sat unanswered for a while means a human is actually needed.
    if waited_ms < AWAITING_PERMISSION_MIN_MS {
        return None;
    }
    Some(json!({
        "count": pending.len(),
        "tools": pending.iter().map(|p| p.event.tool_name.clone()).collect::<Vec<_>>(),
        "since": oldest.requested_at.to_rfc3339(),
        "waited_human": format_waited(waited_ms),
        "note": "Blocked on a permission prompt in the desktop app; it will not progress until the user answers it. Tell the user instead of waiting or cancelling.",
    }))
}

/// Attach `awaiting_permission` to a worker status object when it applies.
fn with_awaiting_permission(mut entry: Value, worker_id: &str) -> Value {
    if let (Some(obj), Some(info)) = (entry.as_object_mut(), awaiting_permission(worker_id)) {
        obj.insert("awaiting_permission".to_string(), info);
    }
    entry
}

/// Route a tool name to its handler. `Ok` payloads are surfaced to the model as
/// JSON; `Err(String)` becomes an MCP `isError` tool result.
pub async fn dispatch(
    state: &HttpState,
    conductor_run_id: &str,
    name: &str,
    args: Value,
) -> Result<Value, String> {
    match name {
        "session_spawn" => session_spawn(state, conductor_run_id, args).await,
        "session_list" => session_list(state, conductor_run_id).await,
        "session_poll" => session_poll(state, conductor_run_id, args).await,
        "session_wait" => session_wait(state, conductor_run_id, args).await,
        "session_read" => session_read(state, conductor_run_id, args).await,
        "session_send" => session_send(state, conductor_run_id, args).await,
        "session_cancel" => session_cancel(state, conductor_run_id, args).await,
        other => Err(format!("unknown tool: {}", other)),
    }
}

fn db(state: &HttpState) -> Db {
    state.app.state::<Db>().inner().clone()
}

fn registry(state: &HttpState) -> RunRegistry {
    state.app.state::<RunRegistry>().inner().clone()
}

fn arg_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

async fn session_spawn(
    state: &HttpState,
    conductor_run_id: &str,
    args: Value,
) -> Result<Value, String> {
    let session = state
        .conductor
        .session(conductor_run_id)
        .ok_or_else(|| "unknown conductor session".to_string())?;

    let prompt = arg_str(&args, "prompt")
        .ok_or_else(|| "`prompt` is required".to_string())?;
    let role_label = arg_str(&args, "role_label").unwrap_or_else(|| "worker".to_string());
    let instruction = arg_str(&args, "instruction");
    // Passed through as the worker's model override. `start_run_inner` resolves it
    // against the WORKER's engine (which may differ from the conductor's) and drops
    // it when it belongs to the other engine, so a cross-engine spawn falls back to
    // that engine's global default instead of leaking the conductor's model.
    let model = arg_str(&args, "model");
    // No override => worker uses the project's default permission mode, exactly
    // like a normal session; its prompts surface in the standard drawer.
    let permission_mode = arg_str(&args, "permission_mode");

    let db = db(state);
    let engine = match arg_str(&args, "engine") {
        Some(e) => e,
        None => crate::commands::settings::resolve_default_engine(&db).await,
    };

    let worker_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let title = truncate(&format!("[{}] {}", role_label, prompt), 80);

    sqlx::query(
        "INSERT INTO runs (id, project_id, type, status, engine, created_at, title, role, conductor_run_id)
         VALUES (?, ?, 'session', 'fetched', ?, ?, ?, 'worker', ?)",
    )
    .bind(&worker_id)
    .bind(&session.project_id)
    .bind(&engine)
    .bind(&now)
    .bind(&title)
    .bind(conductor_run_id)
    .execute(&db)
    .await
    .map_err(|e| e.to_string())?;

    // Reserve the worker slot (enforces the per-conductor cap).
    if !state.conductor.add_worker(conductor_run_id, &worker_id) {
        let _ = sqlx::query("DELETE FROM runs WHERE id = ?")
            .bind(&worker_id)
            .execute(&db)
            .await;
        return Err(format!(
            "worker cap reached ({} max) — read/cancel existing workers first",
            session.max_workers
        ));
    }

    let payload = StartRunPayload {
        run_id: worker_id.clone(),
        engine_override: Some(engine.clone()),
        permission_mode_override: permission_mode,
        prompt_override: Some(prompt.clone()),
        model_override: model,
        images: Vec::new(),
        override_budget: false,
        append_system_prompt: instruction,
        conductor_token: None,
    };

    start_run_inner(state.app.clone(), db.clone(), registry(state), payload).await?;

    log_event(
        state,
        conductor_run_id,
        "spawn",
        json!({ "worker_id": worker_id, "role_label": role_label, "engine": engine }),
    )
    .await;

    Ok(json!({ "worker_id": worker_id, "role_label": role_label, "status": "running" }))
}

async fn session_list(state: &HttpState, conductor_run_id: &str) -> Result<Value, String> {
    let db = db(state);
    let rows = sqlx::query(
        "SELECT id, title, status, engine, role FROM runs WHERE conductor_run_id = ? ORDER BY created_at",
    )
    .bind(conductor_run_id)
    .fetch_all(&db)
    .await
    .map_err(|e| e.to_string())?;

    let workers: Vec<Value> = rows
        .iter()
        .map(|r| {
            let status: String = r.get("status");
            let id: String = r.get("id");
            let entry = json!({
                "worker_id": id,
                "title": r.get::<Option<String>, _>("title"),
                "engine": r.get::<String, _>("engine"),
                "status": normalize_status(&status),
            });
            with_awaiting_permission(entry, &id)
        })
        .collect();
    Ok(json!({ "workers": workers }))
}

async fn session_poll(
    state: &HttpState,
    conductor_run_id: &str,
    args: Value,
) -> Result<Value, String> {
    let ids = requested_ids(state, conductor_run_id, &args).await?;
    let db = db(state);
    let mut out = Vec::new();
    for id in ids {
        let status = worker_status(&db, &id).await?;
        let entry = json!({ "worker_id": id, "status": normalize_status(&status) });
        out.push(with_awaiting_permission(entry, &id));
    }
    Ok(json!({ "workers": out }))
}

/// Resolve + validate the worker ids a `session_wait` call targets. Shared by the
/// blocking fallback below and the streaming (SSE) path in `mcp_http`.
pub(crate) async fn resolve_wait_ids(
    state: &HttpState,
    conductor_run_id: &str,
    args: &Value,
) -> Result<Vec<String>, String> {
    let ids = requested_ids(state, conductor_run_id, args).await?;
    if ids.is_empty() {
        return Err("`worker_ids` is required and must be non-empty".to_string());
    }
    Ok(ids)
}

/// The effective wait ceiling for a call: the requested `timeout_ms` clamped to
/// `[_, MAX_WAIT_MS]`, defaulting to `DEFAULT_WAIT_MS`. This is only the upper
/// bound; the real elapsed time is always reported via `waited_human`.
pub(crate) fn wait_timeout_ms(args: &Value) -> u64 {
    args.get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_WAIT_MS)
        .min(MAX_WAIT_MS)
}

/// One poll pass over `ids`: partitions them into `done` (status blocks) and
/// `pending` (still running). Cheap — a single status read per worker.
pub(crate) async fn wait_snapshot(
    state: &HttpState,
    ids: &[String],
) -> Result<(Vec<Value>, Vec<String>), String> {
    let db = db(state);
    let mut done = Vec::new();
    let mut pending = Vec::new();
    for id in ids {
        let status = worker_status(&db, id).await?;
        if status == "running" {
            pending.push(id.clone());
        } else {
            done.push(json!({ "worker_id": id, "status": normalize_status(&status) }));
        }
    }
    Ok((done, pending))
}

/// Build the terminal `session_wait` result from a final snapshot + real elapsed.
/// Carries `waited_human` + a pointed `note` so the model quotes the real wait,
/// never the `timeout_ms` ceiling.
pub(crate) fn build_wait_result(done: Vec<Value>, pending: Vec<String>, waited_ms: u64) -> Value {
    let timed_out = !pending.is_empty();
    let note = if timed_out {
        format!(
            "You actually waited {} (the worker is still running); report this, \
             NOT the timeout_ms ceiling you passed in.",
            format_waited(waited_ms)
        )
    } else {
        format!(
            "You actually waited {}; report this exact figure as the wait time.",
            format_waited(waited_ms)
        )
    };
    let awaiting: Vec<Value> = pending
        .iter()
        .filter_map(|id| {
            awaiting_permission(id).map(|info| json!({ "worker_id": id, "awaiting_permission": info }))
        })
        .collect();
    let mut result = json!({
        "done": done,
        "pending": pending,
        "timed_out": timed_out,
        "waited_ms": waited_ms,
        "waited_human": format_waited(waited_ms),
        "note": note,
    });
    if !awaiting.is_empty() {
        result["awaiting_permission"] = Value::Array(awaiting);
    }
    result
}

/// Blocking fallback for `session_wait`. The primary path streams progress over
/// SSE (`mcp_http::session_wait_stream`) to keep the tool call alive; this plain
/// version is kept for any non-streaming dispatch and shares the same helpers.
async fn session_wait(
    state: &HttpState,
    conductor_run_id: &str,
    args: Value,
) -> Result<Value, String> {
    let ids = resolve_wait_ids(state, conductor_run_id, &args).await?;
    let timeout_ms = wait_timeout_ms(&args);
    let started = Instant::now();
    let deadline = started + Duration::from_millis(timeout_ms);

    loop {
        let (done, pending) = wait_snapshot(state, &ids).await?;
        if pending.is_empty() || Instant::now() >= deadline {
            let waited_ms = started.elapsed().as_millis() as u64;
            return Ok(build_wait_result(done, pending, waited_ms));
        }
        tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
}

async fn session_read(
    state: &HttpState,
    conductor_run_id: &str,
    args: Value,
) -> Result<Value, String> {
    let worker_id = arg_str(&args, "worker_id")
        .ok_or_else(|| "`worker_id` is required".to_string())?;
    ensure_owned(state, conductor_run_id, &worker_id)?;

    let db = db(state);
    let row = sqlx::query(
        "SELECT r.status, p.path as project_path FROM runs r
         JOIN projects p ON p.id = r.project_id WHERE r.id = ?",
    )
    .bind(&worker_id)
    .fetch_one(&db)
    .await
    .map_err(|e| e.to_string())?;
    let status: String = row.get("status");
    let project_path: String = row.get("project_path");

    let log_path = Path::new(&project_path)
        .join(".devdy")
        .join("runs")
        .join(format!("{}.log", worker_id));
    let reply = extract_latest_reply(&log_path);

    let entry = json!({
        "worker_id": worker_id,
        "status": normalize_status(&status),
        "reply": reply,
    });
    Ok(with_awaiting_permission(entry, &worker_id))
}

async fn session_send(
    state: &HttpState,
    conductor_run_id: &str,
    args: Value,
) -> Result<Value, String> {
    let worker_id = arg_str(&args, "worker_id")
        .ok_or_else(|| "`worker_id` is required".to_string())?;
    let text = arg_str(&args, "text").ok_or_else(|| "`text` is required".to_string())?;
    ensure_owned(state, conductor_run_id, &worker_id)?;

    let db = db(state);
    let reg = registry(state);
    let status = worker_status(&db, &worker_id).await?;

    // A worker's turn ends by exiting its sidecar, so a follow-up first resumes
    // the session, then delivers the message — mirroring the desktop composer.
    if status != "running" {
        crate::commands::runs::resume_run(
            state.app.clone(),
            state.app.state::<Db>(),
            state.app.state::<RunRegistry>(),
            worker_id.clone(),
            None,
            None,
            Some(false),
        )
        .await?;
    }

    crate::commands::runs::send_user_message_inner(
        &db,
        &reg,
        crate::commands::runs::SendUserMessagePayload {
            run_id: worker_id.clone(),
            content: text,
            images: Vec::new(),
            override_budget: false,
        },
    )
    .await?;

    log_event(state, conductor_run_id, "send", json!({ "worker_id": worker_id })).await;
    Ok(json!({ "worker_id": worker_id, "accepted": true, "status": "running" }))
}

async fn session_cancel(
    state: &HttpState,
    conductor_run_id: &str,
    args: Value,
) -> Result<Value, String> {
    let worker_id = arg_str(&args, "worker_id")
        .ok_or_else(|| "`worker_id` is required".to_string())?;
    ensure_owned(state, conductor_run_id, &worker_id)?;

    let db = db(state);
    crate::commands::runs::cancel_run_inner(&registry(state), &db, &worker_id).await?;
    log_event(state, conductor_run_id, "cancel", json!({ "worker_id": worker_id })).await;
    Ok(json!({ "worker_id": worker_id, "status": "cancelled" }))
}

// ---- helpers ---------------------------------------------------------------

/// The worker ids the tool should act on: an explicit `worker_ids` (scoped to
/// this conductor) or all of the conductor's workers.
async fn requested_ids(
    state: &HttpState,
    conductor_run_id: &str,
    args: &Value,
) -> Result<Vec<String>, String> {
    if let Some(arr) = args.get("worker_ids").and_then(|v| v.as_array()) {
        let mut ids = Vec::new();
        for v in arr {
            if let Some(s) = v.as_str() {
                ensure_owned(state, conductor_run_id, s)?;
                ids.push(s.to_string());
            }
        }
        Ok(ids)
    } else {
        Ok(state
            .conductor
            .session(conductor_run_id)
            .map(|s| s.worker_ids)
            .unwrap_or_default())
    }
}

fn ensure_owned(
    state: &HttpState,
    conductor_run_id: &str,
    worker_id: &str,
) -> Result<(), String> {
    if state.conductor.owns_worker(conductor_run_id, worker_id) {
        Ok(())
    } else {
        Err(format!("worker {} is not owned by this conductor", worker_id))
    }
}

async fn worker_status(db: &Db, worker_id: &str) -> Result<String, String> {
    sqlx::query_scalar::<_, String>("SELECT status FROM runs WHERE id = ?")
        .bind(worker_id)
        .fetch_optional(db)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("worker {} not found", worker_id))
}

/// Map DB run status onto the conductor's vocabulary. `running` == turn in
/// progress; everything terminal is idle/awaitable.
fn normalize_status(status: &str) -> &'static str {
    match status {
        "running" => "running",
        "done" => "idle",
        "failed" => "failed",
        "cancelled" => "cancelled",
        _ => "idle",
    }
}

/// Parse the worker's stream-json log and return the assistant text produced
/// since its last user turn. Empty string when nothing is readable yet.
fn extract_latest_reply(log_path: &Path) -> String {
    let content = match std::fs::read_to_string(log_path) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    let mut reply = String::new();
    for line in content.lines() {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("user") => reply.clear(),
            Some("assistant") => {
                if let Some(blocks) = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                {
                    for b in blocks {
                        if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                            if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                                if !reply.is_empty() {
                                    reply.push('\n');
                                }
                                reply.push_str(t);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    reply.trim().to_string()
}

/// Append a control-plane event AND notify the UI. The emit is the push signal
/// that replaces the old 2s `get_conductor_detail` poll: it fires exactly at the
/// discrete points that change a conductor's worker membership / timeline
/// (spawn/send/cancel), so the frontend can refresh on demand instead of on a
/// timer. (Per-worker run status changes are not emitted here — the frontend
/// reads those live from the `liveRuns` store via `run:done:{workerId}`.)
async fn log_event(state: &HttpState, session_id: &str, kind: &str, payload: Value) {
    let _ = sqlx::query(
        "INSERT INTO conductor_events (session_id, ts, kind, payload_json) VALUES (?, ?, ?, ?)",
    )
    .bind(session_id)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(kind)
    .bind(payload.to_string())
    .execute(&db(state))
    .await;

    let _ = state.app.emit(
        &format!("conductor:changed:{}", session_id),
        json!({ "kind": kind }),
    );
}

/// Called (via the wake channel) when ANY run's sidecar exits, and when a
/// conductor's turn ends while its sidecar stays alive. Closes the conductor
/// control loop so the human never has to prod it:
///   • if the run is a conductor, its turn is over: release its auto-wake slot
///     and deliver any wake a worker deferred while it was busy;
///   • if the run is a worker, wake its conductor.
/// A no-op for ordinary (non-conductor, non-worker) runs.
pub async fn on_run_finished(app: &tauri::AppHandle, run_id: &str) {
    let Some(cst) = app.try_state::<crate::conductor::ConductorState>() else {
        return;
    };
    let cst = cst.inner().clone();

    // A finishing conductor frees its slot BEFORE we consider any worker wake, so
    // a worker that finished during the conductor's turn can wake it next.
    if cst.session(run_id).is_some() {
        if cst.turn_ended(run_id) {
            wake_conductor(app, &cst, run_id).await;
        }
        return;
    }

    wake_conductor_for_worker(app, &cst, run_id).await;
}

/// If `worker_run_id` is a worker of a live (tracked) conductor, wake that
/// conductor. No-op when the run isn't a worker or its conductor is a
/// finished/forgotten session.
async fn wake_conductor_for_worker(
    app: &tauri::AppHandle,
    cst: &crate::conductor::ConductorState,
    worker_run_id: &str,
) {
    let db = db_from(app);

    // DB is the source of truth for the role + conductor link.
    let row = match sqlx::query("SELECT role, conductor_run_id FROM runs WHERE id = ?")
        .bind(worker_run_id)
        .fetch_optional(&db)
        .await
    {
        Ok(Some(r)) => r,
        _ => return,
    };
    let role: Option<String> = row.get("role");
    if role.as_deref() != Some("worker") {
        return;
    }
    let Some(conductor_run_id) = row.get::<Option<String>, _>("conductor_run_id") else {
        return;
    };

    // Only wake a conductor we still track in memory — it has a live token so the
    // resumed run keeps its `session_*` tools. A finished/forgotten conductor is
    // left alone.
    if cst.session(&conductor_run_id).is_none() {
        return;
    }

    wake_conductor(app, cst, &conductor_run_id).await;
}

/// Nudge a conductor to read its finished workers. Three cases:
///   • mid-turn → defer; the nudge is delivered when that turn ends (injecting
///     now would collide with the in-flight turn);
///   • turn over but sidecar still alive (DB `running`, e.g. kept up by a
///     backgrounded Bash) → send the nudge straight to the live sidecar;
///   • sidecar exited → resume the conductor, then send the nudge.
/// A burst of workers finishing produces one nudge: the woken conductor polls
/// ALL its workers, covering the rest.
async fn wake_conductor(
    app: &tauri::AppHandle,
    cst: &crate::conductor::ConductorState,
    conductor_run_id: &str,
) {
    let db = db_from(app);

    if cst.defer_wake_if_busy(conductor_run_id) {
        return;
    }
    let alive = match worker_status(&db, conductor_run_id).await {
        Ok(s) => s == "running",
        Err(_) => return,
    };
    if !cst.begin_wake_turn(conductor_run_id) {
        return;
    }

    let nudge = "[conductor] A worker you spawned has finished its turn. \
Call session_poll() or session_list() to see which of your workers are now idle, \
session_read() each finished worker's result, then continue toward the goal \
(send follow-ups, spawn a specialist, or conclude). If the goal is now met, give \
the human your final summary.";

    let outcome = async {
        if !alive {
            crate::commands::runs::resume_run(
                app.clone(),
                app.state::<Db>(),
                app.state::<RunRegistry>(),
                conductor_run_id.to_string(),
                None,
                None,
                Some(false),
            )
            .await?;
        }
        crate::commands::runs::send_user_message_inner(
            &db,
            &registry_from(app),
            crate::commands::runs::SendUserMessagePayload {
                run_id: conductor_run_id.to_string(),
                content: nudge.to_string(),
                images: Vec::new(),
                override_budget: false,
            },
        )
        .await
    }
    .await;

    if let Err(e) = outcome {
        // Couldn't wake now (budget gate, lost race, sidecar shutting down…).
        // Release the slot and keep the wake pending: if the live sidecar was on
        // its way out, its exit (`run:done`) delivers it via a resume instead.
        eprintln!("conductor auto-wake failed for {}: {}", conductor_run_id, e);
        cst.turn_ended(conductor_run_id);
        if alive {
            cst.defer_wake(conductor_run_id);
        }
    }
}

fn db_from(app: &tauri::AppHandle) -> Db {
    app.state::<Db>().inner().clone()
}

fn registry_from(app: &tauri::AppHandle) -> RunRegistry {
    app.state::<RunRegistry>().inner().clone()
}

fn truncate(s: &str, max: usize) -> String {
    let t: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        format!("{}…", t)
    } else {
        t
    }
}
