//! Tauri commands to launch and inspect conductor sessions from the frontend.

use serde::Serialize;
use sqlx::Row;
use tauri::{AppHandle, State};

use super::{ConductorState, CONDUCTOR_SYSTEM_PROMPT};
use crate::commands::runs::{start_run_inner, StartRunPayload};
use crate::db::Db;
use crate::runs::RunRegistry;

const DEFAULT_MAX_WORKERS: usize = 10;

#[derive(Serialize)]
pub struct ConductorStarted {
    pub conductor_run_id: String,
    pub project_id: String,
}

/// Promote an existing (freshly created, `fetched`) session run into a conductor
/// and launch it: mark its role, create the conductor session row, register it
/// with the in-process MCP server (minting a scoped token), then start it with
/// the goal as its first message plus the conductor system prompt + `conductor`
/// MCP tools injected.
///
/// Reusing the run the desktop already opened keeps the whole thing on the normal
/// session screen — the conductor IS a session, it just also drives workers.
///
/// The conductor runs in `bypassPermissions` so it can call its own `session_*`
/// orchestration tools without a modal on every step — it does no file work
/// itself; the workers it spawns run under the normal (permission-gated) modes.
#[tauri::command]
pub async fn start_conductor(
    app: AppHandle,
    db: State<'_, Db>,
    registry: State<'_, RunRegistry>,
    conductor: State<'_, ConductorState>,
    run_id: String,
    goal: String,
    max_workers: Option<u32>,
    model_override: Option<String>,
) -> Result<ConductorStarted, String> {
    let goal = goal.trim().to_string();
    if goal.is_empty() {
        return Err("Hãy nhập mục tiêu cho phiên Conductor.".to_string());
    }

    // Resolve the run's project (and confirm it exists) so the conductor session
    // is scoped correctly.
    let project_id: String = sqlx::query_scalar("SELECT project_id FROM runs WHERE id = ?")
        .bind(&run_id)
        .fetch_optional(db.inner())
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "run not found".to_string())?;

    let now = chrono::Utc::now().to_rfc3339();
    let max_workers = max_workers.map(|m| m as usize).unwrap_or(DEFAULT_MAX_WORKERS);

    sqlx::query("UPDATE runs SET role = 'conductor' WHERE id = ?")
        .bind(&run_id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    sqlx::query(
        "INSERT OR REPLACE INTO conductor_sessions (id, project_id, goal, status, max_workers, created_at)
         VALUES (?, ?, ?, 'running', ?, ?)",
    )
    .bind(&run_id)
    .bind(&project_id)
    .bind(&goal)
    .bind(max_workers as i64)
    .bind(&now)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    let token = conductor
        .inner()
        .register_session(&run_id, &project_id, max_workers);

    let append = format!(
        "{}\n\nYour worker cap for this session is {} concurrent workers.",
        CONDUCTOR_SYSTEM_PROMPT, max_workers
    );

    let payload = StartRunPayload {
        run_id: run_id.clone(),
        engine_override: None,
        permission_mode_override: Some("bypassPermissions".to_string()),
        prompt_override: Some(goal),
        model_override,
        images: Vec::new(),
        override_budget: false,
        append_system_prompt: Some(append),
        conductor_token: Some(token),
    };

    if let Err(e) =
        start_run_inner(app, db.inner().clone(), registry.inner().clone(), payload).await
    {
        let _ = sqlx::query("UPDATE conductor_sessions SET status = 'failed', finished_at = ? WHERE id = ?")
            .bind(chrono::Utc::now().to_rfc3339())
            .bind(&run_id)
            .execute(db.inner())
            .await;
        return Err(e);
    }

    Ok(ConductorStarted {
        conductor_run_id: run_id,
        project_id,
    })
}

/// Convert an already-started, now-stopped normal session into a conductor.
///
/// Unlike [`start_conductor`] (which cold-starts a fresh `fetched` run with the
/// goal as its first message), this operates on a session that has already run
/// and holds a captured `session_id`. It is a pure in-place UPGRADE: it marks the
/// run as a conductor and creates the conductor session row + token, and nothing
/// else. It deliberately does NOT resume the session or send any turn, so the
/// conversion consumes no tokens. The user resumes the session with their own
/// prompt when ready; `resume_run` then re-injects the `conductor` MCP tools
/// because the role is now `conductor` (the token is live for this app session,
/// and `reconnect_running` restores it from the 'running' row after a restart).
///
/// Only stopped sessions can be converted: a running session owns a live query
/// whose options are frozen, and a `fetched` one should use `start_conductor`.
#[tauri::command]
pub async fn convert_run_to_conductor(
    db: State<'_, Db>,
    conductor: State<'_, ConductorState>,
    run_id: String,
    max_workers: Option<u32>,
) -> Result<ConductorStarted, String> {
    let row = sqlx::query(
        "SELECT project_id, status, role, type as run_type, session_id FROM runs WHERE id = ?",
    )
    .bind(&run_id)
    .fetch_optional(db.inner())
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "run not found".to_string())?;

    let project_id: String = row.get("project_id");
    let status: String = row.get("status");
    let role: Option<String> = row.get("role");
    let run_type: String = row.get("run_type");
    let session_id: Option<String> = row.get("session_id");

    if run_type != "session" {
        return Err("Chỉ có thể chuyển session thường sang conductor.".to_string());
    }
    match role.as_deref() {
        Some("conductor") => return Err("Phiên này đã là conductor.".to_string()),
        Some("worker") => return Err("Không thể chuyển một worker thành conductor.".to_string()),
        _ => {}
    }
    if status == "running" {
        return Err("Hãy dừng phiên trước khi chuyển sang conductor.".to_string());
    }
    if status == "fetched" {
        return Err("Phiên chưa khởi chạy — hãy dùng 'Conductor mới' thay vì chuyển đổi.".to_string());
    }
    if session_id.as_deref().map(str::trim).unwrap_or("").is_empty() {
        return Err(
            "Phiên này chưa có session id để tiếp tục — không thể chuyển sang conductor.".to_string(),
        );
    }

    let now = chrono::Utc::now().to_rfc3339();
    let max_workers = max_workers.map(|m| m as usize).unwrap_or(DEFAULT_MAX_WORKERS);

    sqlx::query("UPDATE runs SET role = 'conductor' WHERE id = ?")
        .bind(&run_id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    sqlx::query(
        "INSERT OR REPLACE INTO conductor_sessions (id, project_id, goal, status, max_workers, created_at)
         VALUES (?, ?, '', 'running', ?, ?)",
    )
    .bind(&run_id)
    .bind(&project_id)
    .bind(max_workers as i64)
    .bind(&now)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    // Register the in-memory session + token now so a resume within this app
    // session re-injects the loopback `conductor` MCP server (resume_run looks it
    // up via token_for). After a restart, `reconnect_running` restores it from the
    // 'running' conductor_sessions row.
    let _token = conductor
        .inner()
        .register_session(&run_id, &project_id, max_workers);

    // Deliberately no resume and no auto-sent turn: converting only upgrades the
    // session in the DB. The user resumes it with their own prompt when ready, at
    // which point resume_run injects the conductor tools (role is now 'conductor').

    Ok(ConductorStarted {
        conductor_run_id: run_id,
        project_id,
    })
}

#[derive(Serialize)]
pub struct ConductorSessionRow {
    pub id: String,
    pub project_id: String,
    pub goal: String,
    pub status: String,
    pub max_workers: i64,
    pub worker_count: i64,
    pub created_at: String,
    pub finished_at: Option<String>,
}

#[derive(Serialize)]
pub struct ConductorWorker {
    pub worker_id: String,
    pub title: Option<String>,
    pub status: String,
    pub engine: String,
    /// Resolved model / Claude account snapshotted on the worker run at start,
    /// so the worker panel can show the same badges as the History rows.
    pub model: Option<String>,
    pub claude_account_id: Option<String>,
}

#[derive(Serialize)]
pub struct ConductorEvent {
    pub ts: String,
    pub kind: String,
    pub payload: serde_json::Value,
}

#[derive(Serialize)]
pub struct ConductorDetail {
    pub session: Option<ConductorSessionRow>,
    pub workers: Vec<ConductorWorker>,
    pub events: Vec<ConductorEvent>,
}

/// Full detail for one conductor session: its workers and its control-plane
/// event timeline. Used by the ConductorWorkspace view.
#[tauri::command]
pub async fn get_conductor_detail(
    db: State<'_, Db>,
    conductor_run_id: String,
) -> Result<ConductorDetail, String> {
    let srow = sqlx::query(
        "SELECT s.id, s.project_id, s.goal, s.status, s.max_workers, s.created_at, s.finished_at,
                (SELECT COUNT(*) FROM runs w WHERE w.conductor_run_id = s.id) AS worker_count
         FROM conductor_sessions s WHERE s.id = ?",
    )
    .bind(&conductor_run_id)
    .fetch_optional(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    let session = srow.map(|r| ConductorSessionRow {
        id: r.get("id"),
        project_id: r.get("project_id"),
        goal: r.get("goal"),
        status: r.get("status"),
        max_workers: r.get("max_workers"),
        worker_count: r.get("worker_count"),
        created_at: r.get("created_at"),
        finished_at: r.get("finished_at"),
    });

    // Newest activity first, mirroring the History list's sort (list_runs) so the
    // most recently active worker sits at the top instead of forcing a scroll down.
    let wrows = sqlx::query(
        "SELECT id, title, status, engine, model, claude_account_id FROM runs WHERE conductor_run_id = ?
         ORDER BY COALESCE(last_activity_at, finished_at, started_at, created_at) DESC",
    )
    .bind(&conductor_run_id)
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;
    let workers = wrows
        .iter()
        .map(|r| ConductorWorker {
            worker_id: r.get("id"),
            title: r.get("title"),
            status: r.get("status"),
            engine: r.get("engine"),
            model: r.get("model"),
            claude_account_id: r.get("claude_account_id"),
        })
        .collect();

    let erows = sqlx::query(
        "SELECT ts, kind, payload_json FROM conductor_events WHERE session_id = ? ORDER BY id DESC LIMIT 200",
    )
    .bind(&conductor_run_id)
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;
    let events = erows
        .iter()
        .map(|r| ConductorEvent {
            ts: r.get("ts"),
            kind: r.get("kind"),
            payload: serde_json::from_str(&r.get::<String, _>("payload_json"))
                .unwrap_or(serde_json::Value::Null),
        })
        .collect();

    Ok(ConductorDetail {
        session,
        workers,
        events,
    })
}

/// Bounds for the worker cap, mirrored by the composer/sidebar number inputs.
const MIN_MAX_WORKERS: u32 = 1;
const MAX_MAX_WORKERS: u32 = 100;

/// Change a conductor's concurrent-worker cap while it is running (or after).
///
/// Persists the new cap to `conductor_sessions` (so a resume/reconnect keeps it)
/// and updates the live in-memory session, which `session_spawn` reads under the
/// lock — so raising the cap lets the conductor spawn more workers on its next
/// attempt, and lowering it blocks further spawns past the new limit (already
/// running workers are never killed).
#[tauri::command]
pub async fn set_conductor_max_workers(
    db: State<'_, Db>,
    conductor: State<'_, ConductorState>,
    conductor_run_id: String,
    max_workers: u32,
) -> Result<i64, String> {
    let clamped = max_workers.clamp(MIN_MAX_WORKERS, MAX_MAX_WORKERS) as i64;

    let affected = sqlx::query("UPDATE conductor_sessions SET max_workers = ? WHERE id = ?")
        .bind(clamped)
        .bind(&conductor_run_id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?
        .rows_affected();
    if affected == 0 {
        return Err("conductor session not found".to_string());
    }

    // Best-effort live update: a conductor that finished (or predates a restart)
    // may no longer be in memory — the persisted value above is what a resume
    // would reload, so a missing in-memory session is not an error.
    conductor
        .inner()
        .set_max_workers(&conductor_run_id, clamped as usize);

    Ok(clamped)
}

/// On app startup, restore in-memory ownership for conductor sessions that were
/// still `running` when the app last closed, so a resumed conductor keeps control
/// of its workers (and its `session_*` tools re-inject with a valid token).
pub async fn reconnect_running(db: &Db, conductor: &ConductorState) {
    let sessions = match sqlx::query(
        "SELECT id, project_id, max_workers FROM conductor_sessions WHERE status = 'running'",
    )
    .fetch_all(db)
    .await
    {
        Ok(s) => s,
        Err(_) => return,
    };
    for s in &sessions {
        let id: String = s.get("id");
        let project_id: String = s.get("project_id");
        let max_workers: i64 = s.get("max_workers");
        let worker_ids: Vec<String> =
            sqlx::query("SELECT id FROM runs WHERE conductor_run_id = ?")
                .bind(&id)
                .fetch_all(db)
                .await
                .map(|rows| rows.iter().map(|r| r.get::<String, _>("id")).collect())
                .unwrap_or_default();
        conductor.reregister_session(&id, &project_id, max_workers as usize, worker_ids);
    }
    if !sessions.is_empty() {
        tracing::info!(event = "conductor_reconnected", count = sessions.len());
    }
}
