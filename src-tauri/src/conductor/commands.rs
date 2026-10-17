//! Tauri commands to launch and inspect conductor sessions from the frontend.

use serde::Serialize;
use sqlx::Row;
use tauri::{AppHandle, State};

use super::{ConductorLaunch, ConductorState, DEFAULT_MAX_WORKERS};
use crate::commands::runs::{start_run_inner, ImageAttachment, StartRunPayload};
use crate::db::Db;
use crate::runs::RunRegistry;

/// Run types a conductor can be launched on. Issue/PR runs keep their issue/PR
/// prompt + input file; the conductor framing is added on top.
const CONDUCTOR_RUN_TYPES: &[&str] = &["session", "analyze_issue", "review_pr"];

#[derive(Serialize)]
pub struct ConductorStarted {
    pub conductor_run_id: String,
    pub project_id: String,
}

/// DB-only upgrade of a run into a conductor: mark its role and make sure its
/// `conductor_sessions` row exists and is `running` (so `reconnect_running`
/// restores it after a restart). Idempotent. `goal` / `max_workers` only
/// overwrite the stored values when given.
async fn upgrade_run_to_conductor(
    db: &Db,
    run_id: &str,
    project_id: &str,
    goal: Option<&str>,
    max_workers: Option<usize>,
) -> Result<(), String> {
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query("UPDATE runs SET role = 'conductor' WHERE id = ? AND (role IS NULL OR role != 'conductor')")
        .bind(run_id)
        .execute(db)
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query(
        "INSERT OR IGNORE INTO conductor_sessions (id, project_id, goal, status, max_workers, created_at)
         VALUES (?, ?, '', 'running', ?, ?)",
    )
    .bind(run_id)
    .bind(project_id)
    .bind(max_workers.unwrap_or(DEFAULT_MAX_WORKERS) as i64)
    .bind(&now)
    .execute(db)
    .await
    .map_err(|e| e.to_string())?;
    sqlx::query("UPDATE conductor_sessions SET status = 'running', finished_at = NULL WHERE id = ?")
        .bind(run_id)
        .execute(db)
        .await
        .map_err(|e| e.to_string())?;
    if let Some(goal) = goal.map(str::trim).filter(|g| !g.is_empty()) {
        sqlx::query("UPDATE conductor_sessions SET goal = ? WHERE id = ?")
            .bind(goal)
            .bind(run_id)
            .execute(db)
            .await
            .map_err(|e| e.to_string())?;
    }
    if let Some(m) = max_workers {
        sqlx::query("UPDATE conductor_sessions SET max_workers = ? WHERE id = ?")
            .bind(m as i64)
            .bind(run_id)
            .execute(db)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Record that a run's Codex thread now carries its framing (reported by the
/// Codex sidecar). Best-effort: a missed write only means one extra seed later.
pub(crate) async fn mark_framing_seeded(db: &Db, run_id: &str) {
    let _ = sqlx::query("UPDATE runs SET framing_seeded = 1 WHERE id = ?")
        .bind(run_id)
        .execute(db)
        .await;
}

/// Make `run_id` launch as a conductor and return what the launch needs.
///
/// Every top-level run is a conductor: a run that is not a worker (role !=
/// 'worker' and no `conductor_run_id`) is upgraded in place if it isn't one yet
/// (legacy plain session, or a run created by a path that doesn't know about
/// conductors — Remote Control, rerun, handoff). The in-memory session + token
/// is (re)registered when missing, e.g. after a restart for a session whose
/// row wasn't `running`. Workers get `None` and launch as plain runs.
///
/// Called by `start_run_inner` and `resume_run`, so both engines get the
/// `conductor` MCP server and framing on first launch and on every resume.
pub(crate) async fn ensure_conductor_launch(
    db: &Db,
    cst: &ConductorState,
    run_id: &str,
) -> Result<Option<ConductorLaunch>, String> {
    let row = sqlx::query("SELECT project_id, role, conductor_run_id FROM runs WHERE id = ?")
        .bind(run_id)
        .fetch_optional(db)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "run not found".to_string())?;
    let project_id: String = row.get("project_id");
    let role: Option<String> = row.get("role");
    let parent: Option<String> = row.get("conductor_run_id");
    if role.as_deref() == Some("worker") || parent.as_deref().is_some_and(|p| !p.trim().is_empty()) {
        return Ok(None);
    }

    upgrade_run_to_conductor(db, run_id, &project_id, None, None).await?;

    let max_workers: i64 =
        sqlx::query_scalar("SELECT max_workers FROM conductor_sessions WHERE id = ?")
            .bind(run_id)
            .fetch_one(db)
            .await
            .map_err(|e| e.to_string())?;
    let max_workers = max_workers.max(1) as usize;

    let token = match cst.token_for(run_id) {
        Some(token) => {
            // Keep the live cap in sync with the persisted one (start_conductor
            // may have just changed it).
            cst.set_max_workers(run_id, max_workers);
            token
        }
        None => {
            let worker_ids: Vec<String> =
                sqlx::query_scalar("SELECT id FROM runs WHERE conductor_run_id = ?")
                    .bind(run_id)
                    .fetch_all(db)
                    .await
                    .map_err(|e| e.to_string())?;
            cst.reregister_session(run_id, &project_id, max_workers, worker_ids)
        }
    };

    Ok(Some(ConductorLaunch { token, max_workers }))
}

/// Everything a resume needs to re-apply a run's framing: the conductor launch
/// (top-level runs; `None` for workers), the run's full framing (persisted role
/// instruction and/or session framing), and whether its Codex thread already
/// carries that framing (`runs.framing_seeded`).
pub(crate) struct ResumeFraming {
    pub launch: Option<ConductorLaunch>,
    pub framing: Option<String>,
    pub seeded: bool,
}

pub(crate) async fn resume_framing(
    db: &Db,
    cst: Option<&ConductorState>,
    run_id: &str,
) -> Result<ResumeFraming, String> {
    let launch = match cst {
        Some(cst) => ensure_conductor_launch(db, cst, run_id).await?,
        None => None,
    };
    let row = sqlx::query("SELECT system_instruction, framing_seeded FROM runs WHERE id = ?")
        .bind(run_id)
        .fetch_one(db)
        .await
        .map_err(|e| e.to_string())?;
    let instruction: Option<String> = row.get("system_instruction");
    let seeded: i64 = row.get("framing_seeded");
    Ok(ResumeFraming {
        framing: super::run_framing(instruction.as_deref(), launch.as_ref()),
        launch,
        seeded: seeded != 0,
    })
}

/// Launch a `fetched` run as a conductor. Every session is a conductor, so this
/// is the first-run path for fresh sessions AND Fetch Issue/PR runs, on either
/// engine.
///
/// `goal` is the first message. It is required for a `session`; an issue/PR run
/// may leave it empty to use its default analyze/review prompt (the issue/PR
/// input file reference is always appended by `start_run_inner`).
///
/// Permission mode: `permission_mode_override`, else the global
/// `default_permission_mode` setting, else `default` — exactly like `start_run`.
/// In a gated mode the session's own `conductor` MCP tools are still
/// auto-allowed, so only real file/shell work asks.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn start_conductor(
    app: AppHandle,
    db: State<'_, Db>,
    registry: State<'_, RunRegistry>,
    conductor: State<'_, ConductorState>,
    run_id: String,
    goal: Option<String>,
    max_workers: Option<u32>,
    engine_override: Option<String>,
    model_override: Option<String>,
    permission_mode_override: Option<String>,
    images: Option<Vec<ImageAttachment>>,
    override_budget: Option<bool>,
) -> Result<ConductorStarted, String> {
    let goal = goal.unwrap_or_default().trim().to_string();

    let row = sqlx::query(
        "SELECT project_id, type as run_type, ref_number, role, conductor_run_id FROM runs WHERE id = ?",
    )
    .bind(&run_id)
    .fetch_optional(db.inner())
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "run not found".to_string())?;
    let project_id: String = row.get("project_id");
    let run_type: String = row.get("run_type");
    let ref_number: Option<i64> = row.get("ref_number");
    let role: Option<String> = row.get("role");
    let parent: Option<String> = row.get("conductor_run_id");

    if role.as_deref() == Some("worker") || parent.as_deref().is_some_and(|p| !p.trim().is_empty()) {
        return Err("Không thể khởi chạy một worker như một session.".to_string());
    }
    if !CONDUCTOR_RUN_TYPES.contains(&run_type.as_str()) {
        return Err(format!("Loại run '{}' không hỗ trợ khởi chạy session.", run_type));
    }
    if goal.is_empty() && run_type == "session" {
        return Err("Hãy nhập nội dung công việc để bắt đầu session.".to_string());
    }

    // The stored goal is what the conductor sidebar shows; an issue/PR run with
    // no custom goal is labelled by its reference.
    let stored_goal = if !goal.is_empty() {
        goal.clone()
    } else {
        match (run_type.as_str(), ref_number) {
            ("analyze_issue", Some(n)) => format!("Issue #{}", n),
            ("review_pr", Some(n)) => format!("PR #{}", n),
            _ => String::new(),
        }
    };
    let max_workers = max_workers
        .map(|m| m.clamp(MIN_MAX_WORKERS, MAX_MAX_WORKERS) as usize)
        .unwrap_or(DEFAULT_MAX_WORKERS);

    upgrade_run_to_conductor(
        db.inner(),
        &run_id,
        &project_id,
        Some(&stored_goal),
        Some(max_workers),
    )
    .await?;
    // Register now so the token exists before the sidecar spawns;
    // start_run_inner re-resolves the same session and adds the framing.
    let launch = ensure_conductor_launch(db.inner(), conductor.inner(), &run_id)
        .await?
        .ok_or_else(|| "run cannot be launched as a session".to_string())?;

    let payload = StartRunPayload {
        run_id: run_id.clone(),
        engine_override,
        // None → start_run_inner falls back to the global default mode.
        permission_mode_override: permission_mode_override.filter(|m| !m.trim().is_empty()),
        prompt_override: if goal.is_empty() { None } else { Some(goal) },
        model_override,
        images: images.unwrap_or_default(),
        override_budget: override_budget.unwrap_or(false),
        append_system_prompt: None,
        conductor_token: Some(launch.token),
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

/// Upgrade an existing, stopped top-level run (session or issue/PR) into a
/// conductor. A pure DB upgrade — no resume, no turn, no tokens spent; the
/// conductor tools + framing come online at the next resume.
///
/// Kept for compatibility: since every top-level run is now upgraded
/// automatically (startup migration + on every start/resume), this is mostly a
/// no-op and succeeds idempotently for a run that already is a conductor.
#[tauri::command]
pub async fn convert_run_to_conductor(
    db: State<'_, Db>,
    conductor: State<'_, ConductorState>,
    run_id: String,
    max_workers: Option<u32>,
) -> Result<ConductorStarted, String> {
    let row = sqlx::query(
        "SELECT project_id, status, role, conductor_run_id, type as run_type FROM runs WHERE id = ?",
    )
    .bind(&run_id)
    .fetch_optional(db.inner())
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "run not found".to_string())?;

    let project_id: String = row.get("project_id");
    let status: String = row.get("status");
    let role: Option<String> = row.get("role");
    let parent: Option<String> = row.get("conductor_run_id");
    let run_type: String = row.get("run_type");

    if !CONDUCTOR_RUN_TYPES.contains(&run_type.as_str()) {
        return Err(format!("Loại run '{}' không hỗ trợ khởi chạy session.", run_type));
    }
    if role.as_deref() == Some("worker") || parent.as_deref().is_some_and(|p| !p.trim().is_empty()) {
        return Err("Không thể chuyển một worker thành session.".to_string());
    }
    if role.as_deref() != Some("conductor") && status == "running" {
        return Err("Hãy dừng session trước khi nâng cấp.".to_string());
    }

    let max_workers = max_workers.map(|m| m.clamp(MIN_MAX_WORKERS, MAX_MAX_WORKERS) as usize);
    upgrade_run_to_conductor(db.inner(), &run_id, &project_id, None, max_workers).await?;
    ensure_conductor_launch(db.inner(), conductor.inner(), &run_id).await?;

    Ok(ConductorStarted {
        conductor_run_id: run_id,
        project_id,
    })
}

/// SQL predicate selecting top-level runs that are not conductors yet: not a
/// worker (no role 'worker', no parent conductor) and not currently running.
const LEGACY_TOP_LEVEL_WHERE: &str = "(r.role IS NULL OR r.role NOT IN ('conductor', 'worker'))
     AND (r.conductor_run_id IS NULL OR TRIM(r.conductor_run_id) = '')
     AND r.status != 'running'";

/// One-time-safe startup migration: every legacy plain top-level run becomes a
/// conductor (the same DB upgrade `convert_run_to_conductor` does). Workers and
/// running runs are never touched; runs whose project no longer exists are
/// skipped (the session row's FK needs it). Idempotent — a re-run finds nothing
/// left to convert. Must run before [`reconnect_running`] so the upgraded
/// sessions are registered in memory with a live token.
pub async fn migrate_legacy_sessions(db: &Db) -> Result<u64, String> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut tx = db.begin().await.map_err(|e| e.to_string())?;
    sqlx::query(&format!(
        "INSERT OR IGNORE INTO conductor_sessions (id, project_id, goal, status, max_workers, created_at)
         SELECT r.id, r.project_id, '', 'running', ?, ?
         FROM runs r JOIN projects p ON p.id = r.project_id
         WHERE {LEGACY_TOP_LEVEL_WHERE}"
    ))
    .bind(DEFAULT_MAX_WORKERS as i64)
    .bind(&now)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;
    let converted = sqlx::query(&format!(
        "UPDATE runs SET role = 'conductor'
         WHERE id IN (
           SELECT r.id FROM runs r JOIN conductor_sessions s ON s.id = r.id
           WHERE {LEGACY_TOP_LEVEL_WHERE}
         )"
    ))
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?
    .rows_affected();
    tx.commit().await.map_err(|e| e.to_string())?;
    if converted > 0 {
        tracing::info!(event = "conductor_legacy_sessions_migrated", count = converted);
    }
    Ok(converted)
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
        return Err("session not found".to_string());
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
    // Every top-level run is a conductor now, so this can be many sessions:
    // load all worker links in one query instead of one per session.
    let mut workers_by_conductor: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    if let Ok(rows) = sqlx::query(
        "SELECT id, conductor_run_id FROM runs WHERE conductor_run_id IS NOT NULL",
    )
    .fetch_all(db)
    .await
    {
        for r in &rows {
            let parent: String = r.get("conductor_run_id");
            workers_by_conductor
                .entry(parent)
                .or_default()
                .push(r.get::<String, _>("id"));
        }
    }
    for s in &sessions {
        let id: String = s.get("id");
        // Already registered by a start/resume that raced this startup task:
        // keep its live token.
        if conductor.session(&id).is_some() {
            continue;
        }
        let project_id: String = s.get("project_id");
        let max_workers: i64 = s.get("max_workers");
        let worker_ids = workers_by_conductor.remove(&id).unwrap_or_default();
        conductor.reregister_session(&id, &project_id, max_workers.max(1) as usize, worker_ids);
    }
    if !sessions.is_empty() {
        tracing::info!(event = "conductor_reconnected", count = sessions.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn setup() -> Db {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query("INSERT INTO projects (id, name, path, created_at) VALUES ('p1', 'proj', '/tmp/p1', '2026-01-01')")
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    async fn add_run(db: &Db, id: &str, run_type: &str, status: &str, role: Option<&str>, parent: Option<&str>) {
        sqlx::query(
            "INSERT INTO runs (id, project_id, type, status, engine, created_at, role, conductor_run_id)
             VALUES (?, 'p1', ?, ?, 'codex', '2026-01-01', ?, ?)",
        )
        .bind(id)
        .bind(run_type)
        .bind(status)
        .bind(role)
        .bind(parent)
        .execute(db)
        .await
        .unwrap();
    }

    async fn role_of(db: &Db, id: &str) -> Option<String> {
        sqlx::query_scalar("SELECT role FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(db)
            .await
            .unwrap()
    }

    async fn has_session(db: &Db, id: &str) -> bool {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM conductor_sessions WHERE id = ?")
            .bind(id)
            .fetch_one(db)
            .await
            .unwrap()
            > 0
    }

    #[tokio::test]
    async fn migration_upgrades_only_stopped_top_level_runs_and_is_idempotent() {
        let db = setup().await;
        add_run(&db, "plain", "session", "done", None, None).await;
        add_run(&db, "issue", "analyze_issue", "fetched", None, None).await;
        add_run(&db, "pr", "review_pr", "cancelled", Some("normal"), None).await;
        add_run(&db, "live", "session", "running", None, None).await;
        add_run(&db, "cond", "session", "done", Some("conductor"), None).await;
        add_run(&db, "w1", "session", "done", Some("worker"), Some("cond")).await;
        // Defensive: a run linked to a conductor without the worker role.
        add_run(&db, "w2", "session", "done", None, Some("cond")).await;

        assert_eq!(migrate_legacy_sessions(&db).await.unwrap(), 3);
        for id in ["plain", "issue", "pr"] {
            assert_eq!(role_of(&db, id).await.as_deref(), Some("conductor"), "{id}");
            assert!(has_session(&db, id).await, "{id}");
        }
        assert_eq!(role_of(&db, "live").await, None, "running runs are skipped");
        assert!(!has_session(&db, "live").await);
        assert_eq!(role_of(&db, "w1").await.as_deref(), Some("worker"));
        assert_eq!(role_of(&db, "w2").await, None, "linked runs are never promoted");
        assert!(!has_session(&db, "w1").await && !has_session(&db, "w2").await);
        // The pre-existing conductor keeps its (absent) row: nothing touched it.
        assert!(!has_session(&db, "cond").await);

        // Re-running is harmless.
        assert_eq!(migrate_legacy_sessions(&db).await.unwrap(), 0);

        // Upgraded sessions are restored in memory on startup.
        let cst = ConductorState::new();
        reconnect_running(&db, &cst).await;
        assert!(cst.token_for("plain").is_some());
        assert_eq!(cst.session("plain").unwrap().max_workers, DEFAULT_MAX_WORKERS);
    }

    #[tokio::test]
    async fn ensure_launch_upgrades_top_level_and_skips_workers() {
        let db = setup().await;
        add_run(&db, "s", "review_pr", "done", None, None).await;
        add_run(&db, "w", "session", "fetched", Some("worker"), Some("s")).await;
        let cst = ConductorState::new();

        assert!(ensure_conductor_launch(&db, &cst, "w").await.unwrap().is_none());
        assert_eq!(role_of(&db, "w").await.as_deref(), Some("worker"));

        let first = ensure_conductor_launch(&db, &cst, "s").await.unwrap().unwrap();
        assert_eq!(role_of(&db, "s").await.as_deref(), Some("conductor"));
        assert_eq!(first.max_workers, DEFAULT_MAX_WORKERS);
        // The worker already in the DB is owned after registration.
        assert!(cst.owns_worker("s", "w"));
        // A second launch (resume) reuses the live token and picks up a new cap.
        sqlx::query("UPDATE conductor_sessions SET max_workers = 3 WHERE id = 's'")
            .execute(&db)
            .await
            .unwrap();
        let again = ensure_conductor_launch(&db, &cst, "s").await.unwrap().unwrap();
        assert_eq!(again.token, first.token);
        assert_eq!(again.max_workers, 3);
        assert_eq!(cst.session("s").unwrap().max_workers, 3);
    }

    #[tokio::test]
    async fn ensure_launch_revives_failed_session_after_restart() {
        let db = setup().await;
        add_run(&db, "c", "session", "error", Some("conductor"), None).await;
        sqlx::query(
            "INSERT INTO conductor_sessions (id, project_id, goal, status, max_workers, created_at)
             VALUES ('c', 'p1', 'g', 'failed', 4, '2026-01-01')",
        )
        .execute(&db)
        .await
        .unwrap();
        // Fresh process: reconnect skips the 'failed' row.
        let cst = ConductorState::new();
        reconnect_running(&db, &cst).await;
        assert!(cst.token_for("c").is_none());

        let launch = ensure_conductor_launch(&db, &cst, "c").await.unwrap().unwrap();
        assert_eq!(launch.max_workers, 4);
        assert_eq!(cst.token_for("c").as_deref(), Some(launch.token.as_str()));
        let status: String = sqlx::query_scalar("SELECT status FROM conductor_sessions WHERE id = 'c'")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(status, "running");
        let goal: String = sqlx::query_scalar("SELECT goal FROM conductor_sessions WHERE id = 'c'")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(goal, "g", "a resume must not clobber the stored goal");
    }

    #[tokio::test]
    async fn worker_instruction_is_reapplied_on_resume() {
        let db = setup().await;
        add_run(&db, "s", "session", "done", Some("conductor"), None).await;
        add_run(&db, "w", "session", "done", Some("worker"), Some("s")).await;
        // session_spawn persists the instruction on the worker row.
        sqlx::query("UPDATE runs SET system_instruction = 'You are an independent reviewer.' WHERE id = 'w'")
            .execute(&db)
            .await
            .unwrap();
        let cst = ConductorState::new();

        let w = resume_framing(&db, Some(&cst), "w").await.unwrap();
        assert!(w.launch.is_none(), "a worker never gets the session tools");
        assert_eq!(w.framing.as_deref(), Some("You are an independent reviewer."));

        let s = resume_framing(&db, Some(&cst), "s").await.unwrap();
        let launch = s.launch.expect("top-level run resumes as a session");
        assert_eq!(s.framing, Some(super::super::conductor_framing(launch.max_workers)));
    }

    #[tokio::test]
    async fn codex_legacy_thread_is_seeded_once_then_never_again() {
        let db = setup().await;
        // A legacy plain Codex session, migrated: its thread never saw framing.
        add_run(&db, "legacy", "session", "done", None, None).await;
        migrate_legacy_sessions(&db).await.unwrap();
        let cst = ConductorState::new();

        let r = resume_framing(&db, Some(&cst), "legacy").await.unwrap();
        assert!(!r.seeded);
        let seed = super::super::codex_resume_seed(r.framing.as_deref(), r.seeded)
            .expect("first resume seeds the framing into the prompt");
        assert!(seed.contains(super::super::CONDUCTOR_SYSTEM_PROMPT));

        // The sidecar confirmed the seeded turn (`_devdy_framing_seeded`).
        mark_framing_seeded(&db, "legacy").await;
        let r = resume_framing(&db, Some(&cst), "legacy").await.unwrap();
        assert!(r.seeded);
        assert_eq!(super::super::codex_resume_seed(r.framing.as_deref(), r.seeded), None);
    }

    #[tokio::test]
    async fn deleting_a_session_deletes_its_workers_and_revokes_tokens() {
        let dir = std::env::temp_dir().join(format!("devdy-del-{}", uuid::Uuid::new_v4()));
        let runs_dir = dir.join(".devdy").join("runs");
        std::fs::create_dir_all(&runs_dir).unwrap();
        let db = setup().await;
        sqlx::query("UPDATE projects SET path = ? WHERE id = 'p1'")
            .bind(dir.to_string_lossy().to_string())
            .execute(&db)
            .await
            .unwrap();
        add_run(&db, "s", "session", "done", Some("conductor"), None).await;
        add_run(&db, "w1", "session", "done", Some("worker"), Some("s")).await;
        add_run(&db, "w2", "session", "error", Some("worker"), Some("s")).await;
        add_run(&db, "other", "session", "done", Some("conductor"), None).await;
        for id in ["s", "w1", "w2", "other"] {
            std::fs::write(runs_dir.join(format!("{id}.log")), "x").unwrap();
        }
        let cst = ConductorState::new();
        let token = ensure_conductor_launch(&db, &cst, "s").await.unwrap().unwrap().token;
        let registry = crate::runs::new_registry();

        // A running worker blocks the whole delete — nothing is removed.
        sqlx::query("UPDATE runs SET status = 'running' WHERE id = 'w2'")
            .execute(&db)
            .await
            .unwrap();
        assert!(crate::commands::runs::delete_run_tree(&db, &registry, Some(&cst), "s")
            .await
            .is_err());
        assert!(runs_dir.join("w1.log").exists());
        assert!(cst.resolve_token(&token).is_some());
        sqlx::query("UPDATE runs SET status = 'error' WHERE id = 'w2'")
            .execute(&db)
            .await
            .unwrap();

        // A session stuck 'running' in the DB (e.g. after a crash) is refused
        // BEFORE any worker is touched.
        sqlx::query("UPDATE runs SET status = 'running' WHERE id = 's'")
            .execute(&db)
            .await
            .unwrap();
        assert!(crate::commands::runs::delete_run_tree(&db, &registry, Some(&cst), "s")
            .await
            .is_err());
        let still: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE conductor_run_id = 's'")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(still, 2, "workers must survive a refused delete");
        assert!(runs_dir.join("w1.log").exists() && runs_dir.join("w2.log").exists());
        sqlx::query("UPDATE runs SET status = 'done' WHERE id = 's'")
            .execute(&db)
            .await
            .unwrap();

        let n = crate::commands::runs::delete_run_tree(&db, &registry, Some(&cst), "s")
            .await
            .unwrap();
        assert_eq!(n.deleted, 3);
        let left: Vec<String> = sqlx::query_scalar("SELECT id FROM runs ORDER BY id")
            .fetch_all(&db)
            .await
            .unwrap();
        assert_eq!(left, vec!["other".to_string()]);
        for id in ["s", "w1", "w2"] {
            assert!(!runs_dir.join(format!("{id}.log")).exists(), "{id} log left behind");
        }
        assert!(runs_dir.join("other.log").exists());
        assert!(!has_session(&db, "s").await);
        assert!(cst.session("s").is_none());
        assert!(cst.resolve_token(&token).is_none(), "session token must be revoked");
        let _ = std::fs::remove_dir_all(&dir);
    }

}
