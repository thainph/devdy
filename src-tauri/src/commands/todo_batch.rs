//! Todo batches: hand a set of todos (across projects) to one Todo Conductor
//! session, and the data + actions behind the Batch Board that tracks it.
//!
//! The conductor is an ordinary top-level session (every one is a conductor)
//! flagged `runs.todo_batch = 1`; its workers run in the todos' projects and keep
//! their changes apart as the batch's isolation says: following each project's
//! own worktree rules (default), in Devdy worktrees, or one at a time.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sqlx::Row;
use tauri::{AppHandle, Emitter, State};

use crate::commands::runs::{create_session_run_inner, start_run_inner, StartRunPayload};
use crate::commands::todos::{get_todo, Todo};
use crate::db::Db;
use crate::focus::ai::{batch_prompt, BatchIsolation, BatchOptions, BatchTodo};
use crate::runs::RunRegistry;
use crate::worktrees;

/// Emitted after a board action changed a worktree or a todo.
pub const BOARD_CHANGED_EVENT: &str = "board:changed";

/// Batch options picked on the board (shared by every project's conductor).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardRunOptions {
    pub max_parallel: Option<usize>,
    /// `rules` (default: workers follow their project's worktree rules), `devdy`
    /// (Devdy creates a worktree per worker) or `none`.
    pub isolation: Option<String>,
    pub engine: Option<String>,
    pub permission_mode: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartedBatch {
    pub run_id: String,
    pub project_id: String,
    pub project_name: String,
    pub todo_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedBatch {
    pub project_id: String,
    pub project_name: String,
    pub error: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedTodo {
    pub id: String,
    /// `not_found` | `done` | `no_project` | `in_progress`.
    pub reason: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardRunResult {
    pub started: Vec<StartedBatch>,
    pub failed: Vec<FailedBatch>,
    pub skipped: Vec<SkippedTodo>,
}

static BOARD_RUN_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Run queued todos: they're grouped by project and each project gets its own
/// Todo Conductor, living in that project, whose workers stay in it (enforced by
/// the conductor's `session_spawn`). A project that fails to start doesn't stop
/// the others; it's reported in `failed`.
#[tauri::command]
pub async fn board_run(
    app: AppHandle,
    db: State<'_, Db>,
    registry: State<'_, RunRegistry>,
    todo_ids: Vec<String>,
    options: Option<BoardRunOptions>,
) -> Result<BoardRunResult, String> {
    let db = db.inner().clone();
    let opts = options.unwrap_or_default();
    // One run at a time: two concurrent runs could both find a todo free.
    let _guard = BOARD_RUN_LOCK.lock().await;
    let (todos, skipped) = runnable_todos(&db, &todo_ids).await?;
    if todo_ids.is_empty() {
        return Err("no todos selected".into());
    }
    if todos.is_empty() {
        // Nothing runnable: the caller shows why, todo by todo.
        return Ok(BoardRunResult { started: Vec::new(), failed: Vec::new(), skipped });
    }
    let names = project_names(&db).await?;
    let mut started = Vec::new();
    let mut failed = Vec::new();
    for (project_id, group) in group_by_project(todos) {
        let project_name = names.get(&project_id).cloned().unwrap_or_else(|| project_id.clone());
        match start_project_batch(&app, &db, registry.inner(), &project_id, &project_name, &group, &opts).await {
            Ok(run_id) => started.push(StartedBatch { run_id, project_id, project_name, todo_count: group.len() }),
            Err(error) => failed.push(FailedBatch { project_id, project_name, error }),
        }
    }
    let _ = app.emit(BOARD_CHANGED_EVENT, ());
    Ok(BoardRunResult { started, failed, skipped })
}

/// The todos that can run (open, with a project, not already being worked on in
/// a batch), and why the others can't.
async fn runnable_todos(db: &Db, ids: &[String]) -> Result<(Vec<Todo>, Vec<SkippedTodo>), String> {
    let mut ok = Vec::new();
    let mut skipped = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for id in ids {
        if !seen.insert(id.as_str()) {
            continue;
        }
        let reason = match get_todo(db, id).await {
            Err(_) => Some("not_found"),
            Ok(t) if t.done => Some("done"),
            Ok(t) if t.project_id.is_none() => Some("no_project"),
            Ok(t) => {
                if in_progress(db, &t.id).await? {
                    Some("in_progress")
                } else {
                    ok.push(t);
                    None
                }
            }
        };
        if let Some(r) = reason {
            skipped.push(SkippedTodo { id: id.clone(), reason: r.into() });
        }
    }
    Ok((ok, skipped))
}

/// SQL (todo id = `?1`): a session is working on the todo right now, or it sits
/// in a batch that is alive (its conductor or one of its workers running). Such
/// a todo can't move to another project (`set_todo_project`, and the MCP
/// `todos_set_project` mirrors it): its workers run in the current one.
pub const TODO_BUSY_SQL: &str = "EXISTS (
            SELECT 1 FROM todo_runs ir JOIN runs r ON r.id = ir.run_id
            WHERE ir.todo_id = ?1 AND ir.source = 'created_from_item' AND r.status IN ('running', 'fetched')
         ) OR EXISTS (
            SELECT 1 FROM todo_batch_items b JOIN todos t ON t.id = b.todo_id
            WHERE b.todo_id = ?1 AND t.done = 0 AND (
                EXISTS (SELECT 1 FROM runs c WHERE c.id = b.batch_run_id AND c.status IN ('running', 'fetched'))
                OR EXISTS (SELECT 1 FROM runs w WHERE w.conductor_run_id = b.batch_run_id AND w.status IN ('running', 'fetched'))
            )
         )";

/// Running the todo again would put two workers on it: a session works on it
/// right now, a batch has it waiting for review, or it still waits for a worker
/// in a batch that is alive (its conductor or one of its workers running).
async fn in_progress(db: &Db, todo_id: &str) -> Result<bool, String> {
    sqlx::query_scalar::<_, i64>(
        "SELECT EXISTS (
            SELECT 1 FROM todo_runs ir JOIN runs r ON r.id = ir.run_id
            WHERE ir.todo_id = ?1 AND ir.source = 'created_from_item' AND r.status IN ('running', 'fetched')
         ) OR EXISTS (
            SELECT 1 FROM todo_batch_items b JOIN todos t ON t.id = b.todo_id
            WHERE b.todo_id = ?1 AND t.done = 0 AND (
                t.status IN ('doing', 'waiting')
                OR EXISTS (SELECT 1 FROM runs c WHERE c.id = b.batch_run_id AND c.status IN ('running', 'fetched'))
                OR EXISTS (SELECT 1 FROM runs w WHERE w.conductor_run_id = b.batch_run_id AND w.status IN ('running', 'fetched'))
            )
         )",
    )
    .bind(todo_id)
    .fetch_one(db)
    .await
    .map(|n| n != 0)
    .map_err(|e| e.to_string())
}

/// Todos by project, projects in the order first seen, todos in input order.
fn group_by_project(todos: Vec<Todo>) -> Vec<(String, Vec<Todo>)> {
    let mut out: Vec<(String, Vec<Todo>)> = Vec::new();
    for t in todos {
        let Some(p) = t.project_id.clone() else { continue };
        match out.iter_mut().find(|(k, _)| *k == p) {
            Some((_, list)) => list.push(t),
            None => out.push((p, vec![t])),
        }
    }
    out
}

/// Start one Todo Conductor in `project_id` on `todos` (all of that project);
/// returns its run id.
async fn start_project_batch(
    app: &AppHandle,
    db: &Db,
    registry: &RunRegistry,
    project_id: &str,
    project_name: &str,
    todos: &[Todo],
    opts: &BoardRunOptions,
) -> Result<String, String> {
    // The invariant the conductor relies on: one project per batch.
    if let Some(t) = todos.iter().find(|t| t.project_id.as_deref() != Some(project_id)) {
        return Err(format!("wrong_project: todo {} is not in this project", t.id));
    }
    let max_parallel = opts.max_parallel.unwrap_or(3).clamp(1, crate::conductor::DEFAULT_MAX_WORKERS);
    let batch: Vec<BatchTodo> = todos
        .iter()
        .map(|t| BatchTodo { id: &t.id, text: &t.text, source_url: t.source_url.as_deref() })
        .collect();
    let prompt = batch_prompt(
        &batch,
        &BatchOptions {
            project: project_name,
            max_parallel,
            isolation: BatchIsolation::parse(opts.isolation.as_deref()),
            engine: opts.engine.as_deref().filter(|e| !e.is_empty()),
            permission_mode: opts.permission_mode.as_deref().filter(|m| !m.is_empty()),
        },
    );

    let run = create_session_run_inner(db, project_id.to_string(), None).await?;
    let title = format!("🗂 {} · {}", project_name, batch_title(todos));
    // Batch metadata in one transaction, after checking the todos again on fresh
    // data (one may have moved to another project since it was read).
    let written: Result<(), String> = async {
        let mut tx = db.begin().await.map_err(|e| e.to_string())?;
        for t in todos {
            let now_in: Option<Option<String>> = sqlx::query_scalar("SELECT project_id FROM todos WHERE id = ? AND done = 0")
                .bind(&t.id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
            if now_in.flatten().as_deref() != Some(project_id) {
                return Err(format!("wrong_project: todo {} moved or was closed — run again", t.id));
            }
        }
        sqlx::query("UPDATE runs SET title = ?, todo_batch = 1 WHERE id = ?")
            .bind(crate::focus::ai_util::truncate(&title, 80))
            .bind(&run.id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        for (i, t) in todos.iter().enumerate() {
            sqlx::query("INSERT OR IGNORE INTO todo_batch_items (batch_run_id, todo_id, position) VALUES (?, ?, ?)")
                .bind(&run.id)
                .bind(&t.id)
                .bind(i as i64)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        }
        tx.commit().await.map_err(|e| e.to_string())
    }
    .await;
    if let Err(e) = written {
        discard_batch_run(db, &run.id).await;
        return Err(e);
    }
    let payload = StartRunPayload {
        run_id: run.id.clone(),
        engine_override: None,
        permission_mode_override: None,
        prompt_override: Some(prompt),
        model_override: None,
        images: Vec::new(),
        override_budget: false,
        append_system_prompt: None,
        conductor_token: None,
    };
    if let Err(e) = start_run_inner(app.clone(), db.clone(), registry.clone(), payload).await {
        discard_batch_run(db, &run.id).await;
        return Err(e);
    }
    // Started: these todos leave the board queue. The batch already runs, so a
    // failure here only leaves them listed (and refused as in progress).
    for t in todos {
        if let Err(e) = sqlx::query("DELETE FROM board_queue WHERE todo_id = ?").bind(&t.id).execute(db).await {
            tracing::warn!(event = "board_queue_clear_failed", todo_id = %t.id, error = %e);
        }
    }
    Ok(run.id)
}

/// A batch conductor that never ran: remove it and its items.
async fn discard_batch_run(db: &Db, run_id: &str) {
    for sql in [
        "DELETE FROM conductor_sessions WHERE id = ?",
        "DELETE FROM todo_batch_items WHERE batch_run_id = ?",
        "DELETE FROM runs WHERE id = ?",
    ] {
        if let Err(e) = sqlx::query(sql).bind(run_id).execute(db).await {
            tracing::warn!(event = "batch_discard_failed", run_id, error = %e);
        }
    }
}

/// `Fix login` / `Fix login +2`: the first todo's first line, and how many more.
fn batch_title(todos: &[Todo]) -> String {
    let first = todos.first().map(|t| t.text.lines().next().unwrap_or("").trim()).unwrap_or("");
    let first = crate::focus::ai_util::truncate(first, 48);
    match todos.len() {
        0 | 1 => first,
        n => format!("{first} +{}", n - 1),
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedTodo {
    pub todo: BoardTodo,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub added_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueAddResult {
    pub added: usize,
    pub skipped: Vec<SkippedTodo>,
}

/// "Add to Batch Board": queue todos to be run from the board. Todos without a
/// project (their session needs one), done ones and ones already being worked
/// on in a batch are skipped and reported.
#[tauri::command]
pub async fn board_queue_add(app: AppHandle, db: State<'_, Db>, todo_ids: Vec<String>) -> Result<QueueAddResult, String> {
    let (todos, skipped) = runnable_todos(db.inner(), &todo_ids).await?;
    let now = chrono::Utc::now().to_rfc3339();
    let mut added = 0;
    for t in &todos {
        // Already queued ones are kept as they are (and not counted).
        added += sqlx::query("INSERT OR IGNORE INTO board_queue (todo_id, added_at) VALUES (?, ?)")
            .bind(&t.id)
            .bind(&now)
            .execute(db.inner())
            .await
            .map_err(|e| e.to_string())?
            .rows_affected() as usize;
    }
    if added > 0 {
        let _ = app.emit(BOARD_CHANGED_EVENT, ());
    }
    Ok(QueueAddResult { added, skipped })
}

#[tauri::command]
pub async fn board_queue_remove(app: AppHandle, db: State<'_, Db>, todo_id: String) -> Result<(), String> {
    sqlx::query("DELETE FROM board_queue WHERE todo_id = ?")
        .bind(&todo_id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    let _ = app.emit(BOARD_CHANGED_EVENT, ());
    Ok(())
}

/// The queue in the order todos were added; done todos drop out.
#[tauri::command]
pub async fn board_queue_list(db: State<'_, Db>) -> Result<Vec<QueuedTodo>, String> {
    load_queue(db.inner()).await
}

pub async fn load_queue(db: &Db) -> Result<Vec<QueuedTodo>, String> {
    let rows = sqlx::query(
        "SELECT t.id, t.text, t.status, t.done, t.project_id, p.name AS project_name, q.added_at
         FROM board_queue q JOIN todos t ON t.id = q.todo_id
         LEFT JOIN projects p ON p.id = t.project_id
         WHERE t.done = 0 ORDER BY q.added_at",
    )
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows
        .iter()
        .map(|r| QueuedTodo {
            todo: BoardTodo {
                id: r.get("id"),
                text: r.get("text"),
                status: r.get("status"),
                done: r.get::<i64, _>("done") != 0,
            },
            project_id: r.get("project_id"),
            project_name: r.get("project_name"),
            added_at: r.get("added_at"),
        })
        .collect())
}

async fn project_names(db: &Db) -> Result<HashMap<String, String>, String> {
    let rows = sqlx::query("SELECT id, name FROM projects")
        .fetch_all(db)
        .await
        .map_err(|e| e.to_string())?;
    Ok(rows.iter().map(|r| (r.get("id"), r.get("name"))).collect())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchSummary {
    pub run_id: String,
    pub title: Option<String>,
    pub project_id: String,
    pub project_name: String,
    pub status: String,
    pub created_at: String,
    pub worker_count: i64,
}

/// Todo batches, newest first.
#[tauri::command]
pub async fn board_batches(db: State<'_, Db>) -> Result<Vec<BatchSummary>, String> {
    load_batches(db.inner()).await
}

/// Pseudo batch id for the todo sessions started one by one ("Start with AI"),
/// outside any batch. Listed first on the board when there are any.
pub const SOLO_BATCH: &str = "solo";

/// Runs started on a todo outside any batch, recent ones first (`r` = runs).
const SOLO_WHERE: &str = "r.todo_batch = 0 AND r.conductor_run_id IS NULL AND r.type = 'session'
         AND r.created_at >= datetime('now', '-30 days')
         AND EXISTS (SELECT 1 FROM todo_runs ir WHERE ir.run_id = r.id AND ir.source = 'created_from_item')";

/// `board_batches`, shared with Remote Control.
/// A batch still going on or waiting for the user — the SQL twin of
/// `group_active`: a session of it running (its conductor or a worker), or a
/// worker's todo being worked on / waiting for review.
const OPEN_BATCH: &str = "r.status IN ('running', 'fetched')
         OR EXISTS (SELECT 1 FROM runs w WHERE w.conductor_run_id = r.id AND w.status IN ('running', 'fetched'))
         OR EXISTS (SELECT 1 FROM runs w JOIN todo_runs ir ON ir.run_id = w.id AND ir.source = 'created_from_item'
                    JOIN todos t ON t.id = ir.todo_id
                    WHERE w.conductor_run_id = r.id AND t.done = 0 AND t.status IN ('doing', 'waiting'))";

/// The 30 newest batches plus every older one still open, so a long-running
/// batch never drops off the board (desktop and Remote Control alike).
pub async fn load_batches(db: &Db) -> Result<Vec<BatchSummary>, String> {
    let mut out = Vec::new();
    let solo = sqlx::query(&format!(
        "SELECT COUNT(*) AS n, MAX(r.created_at) AS latest FROM runs r WHERE {SOLO_WHERE}"
    ))
    .fetch_one(db)
    .await
    .map_err(|e| e.to_string())?;
    let solo_count: i64 = solo.get("n");
    if solo_count > 0 {
        out.push(BatchSummary {
            run_id: SOLO_BATCH.to_string(),
            title: None,
            project_id: String::new(),
            project_name: String::new(),
            status: String::new(),
            created_at: solo.get::<Option<String>, _>("latest").unwrap_or_default(),
            worker_count: solo_count,
        });
    }
    let newest = "r.id IN (SELECT id FROM runs WHERE todo_batch = 1 ORDER BY created_at DESC LIMIT 30)";
    let filter = format!("{newest} OR {OPEN_BATCH}");
    let rows = sqlx::query(&format!(
        "SELECT r.id, r.title, r.project_id, p.name AS project_name, r.status, r.created_at,
                (SELECT COUNT(*) FROM runs w WHERE w.conductor_run_id = r.id) AS worker_count
         FROM runs r JOIN projects p ON p.id = r.project_id
         WHERE r.todo_batch = 1 AND ({filter}) ORDER BY r.created_at DESC"
    ))
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())?;
    out.extend(rows.iter().map(|r| BatchSummary {
            run_id: r.get("id"),
            title: r.get("title"),
            project_id: r.get("project_id"),
            project_name: r.get("project_name"),
            status: r.get("status"),
            created_at: r.get("created_at"),
            worker_count: r.get("worker_count"),
        }));
    Ok(out)
}

#[derive(Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DiffStat {
    /// Commits on the branch since it left its base.
    pub commits: i64,
    pub files: i64,
    pub insertions: i64,
    pub deletions: i64,
    /// Changed files not committed yet.
    pub uncommitted: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardTodo {
    pub id: String,
    pub text: String,
    pub status: String,
    pub done: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardItem {
    pub run_id: String,
    pub title: Option<String>,
    /// `conductor` for the batch's own session, else `worker`.
    pub role: String,
    pub project_id: String,
    pub project_name: String,
    pub engine: String,
    /// `runs.status`: fetched | running | done | failed | cancelled.
    pub status: String,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub cost_usd: f64,
    pub todo: Option<BoardTodo>,
    pub worktree: Option<worktrees::Worktree>,
    pub diff: Option<DiffStat>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardData {
    pub items: Vec<BoardItem>,
    /// Todos of the batch no worker has picked up yet, in batch order.
    pub queued: Vec<BoardQueuedTodo>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardQueuedTodo {
    pub todo: BoardTodo,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
}

/// The batch's conductor and every worker it spawned (with todo, worktree and
/// diff), plus the todos still queued. Live state (permission prompts,
/// streaming) comes from the run events.
#[tauri::command]
pub async fn board_items(db: State<'_, Db>, batch_run_id: String) -> Result<BoardData, String> {
    load_board(db.inner(), &batch_run_id).await
}

/// `board_items`, shared with Remote Control.
pub async fn load_board(db: &Db, batch_run_id: &str) -> Result<BoardData, String> {
    load_board_with(db, batch_run_id, true).await
}

/// `load_board`; `with_diff = false` skips the git calls (finished batches).
async fn load_board_with(db: &Db, batch_run_id: &str, with_diff: bool) -> Result<BoardData, String> {
    let batch_run_id = batch_run_id.to_string();
    // A batch: its conductor first, then its workers. The solo pseudo batch:
    // every todo session started on its own, newest first.
    let (filter, order) = if batch_run_id == SOLO_BATCH {
        (SOLO_WHERE.to_string(), "r.created_at DESC LIMIT 50")
    } else {
        ("(r.id = ?1 OR r.conductor_run_id = ?1)".to_string(), "r.id != ?1, r.created_at")
    };
    let sql = format!(
        "SELECT r.id, r.title, r.role, r.project_id, p.name AS project_name, r.engine, r.status,
                r.created_at, r.started_at, r.finished_at, r.worktree_path,
                (SELECT CAST(COALESCE(SUM(u.cost_usd), 0) AS REAL) FROM run_usage u WHERE u.run_id = r.id) AS cost,
                (SELECT ir.todo_id FROM todo_runs ir WHERE ir.run_id = r.id AND ir.source = 'created_from_item' ORDER BY ir.linked_at LIMIT 1) AS todo_id
         FROM runs r JOIN projects p ON p.id = r.project_id
         WHERE {filter}
         ORDER BY {order}"
    );
    let query = sqlx::query(&sql);
    // The solo filter has no placeholder to bind.
    let query = if batch_run_id == SOLO_BATCH { query } else { query.bind(&batch_run_id) };
    let rows = query.fetch_all(db).await.map_err(|e| e.to_string())?;

    let mut items = Vec::new();
    for r in rows {
        let run_id: String = r.get("id");
        let todo = match r.get::<Option<String>, _>("todo_id") {
            Some(id) => get_todo(db, &id).await.ok().map(|t| BoardTodo {
                id: t.id,
                text: t.text,
                status: t.status,
                done: t.done,
            }),
            None => None,
        };
        let wt = match r.get::<Option<String>, _>("worktree_path") {
            Some(path) => worktree_by_path(db, &path).await,
            None => None,
        };
        let diff = match &wt {
            Some(w) if with_diff && w.removed_at.is_none() && Path::new(&w.path).is_dir() => Some(diff_stat(w).await),
            _ => None,
        };
        items.push(BoardItem {
            role: if run_id == batch_run_id { "conductor".into() } else { "worker".into() },
            run_id,
            title: r.get("title"),
            project_id: r.get("project_id"),
            project_name: r.get("project_name"),
            engine: r.get("engine"),
            status: r.get("status"),
            created_at: r.get("created_at"),
            started_at: r.get("started_at"),
            finished_at: r.get("finished_at"),
            // Never panic on a decode: a missing / odd value just shows no cost.
            cost_usd: r.try_get::<Option<f64>, _>("cost").ok().flatten().unwrap_or(0.0),
            todo,
            worktree: wt,
            diff,
        });
    }

    let picked: std::collections::HashSet<String> =
        items.iter().filter_map(|i| i.todo.as_ref().map(|t| t.id.clone())).collect();
    let rows = sqlx::query(
        "SELECT t.id, t.text, t.status, t.done, t.project_id, p.name AS project_name
         FROM todo_batch_items b JOIN todos t ON t.id = b.todo_id
         LEFT JOIN projects p ON p.id = t.project_id
         WHERE b.batch_run_id = ? ORDER BY b.position",
    )
    .bind(&batch_run_id)
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())?;
    let queued = rows
        .iter()
        .filter(|r| !picked.contains(&r.get::<String, _>("id")))
        .map(|r| BoardQueuedTodo {
            todo: BoardTodo {
                id: r.get("id"),
                text: r.get("text"),
                status: r.get("status"),
                done: r.get::<i64, _>("done") != 0,
            },
            project_id: r.get("project_id"),
            project_name: r.get("project_name"),
        })
        .collect();
    Ok(BoardData { items, queued })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardGroup {
    pub batch: BatchSummary,
    #[serde(flatten)]
    pub data: BoardData,
    /// Something is still going on or waits for the user: a session running,
    /// a todo being worked on / waiting for review, or todos still queued in a
    /// running batch. Finished groups go to the board's history.
    pub active: bool,
}

/// Every batch (one per project and run) with its sessions, active ones first,
/// then newest first; plus the solo todo sessions as their own group.
#[tauri::command]
pub async fn board_overview(db: State<'_, Db>) -> Result<Vec<BoardGroup>, String> {
    load_overview(db.inner()).await
}

pub async fn load_overview(db: &Db) -> Result<Vec<BoardGroup>, String> {
    let mut groups = Vec::new();
    for batch in load_batches(db).await? {
        let mut data = load_board_with(db, &batch.run_id, false).await?;
        let active = group_active(&data);
        // Finished batches are shown without diffs: no git calls for history.
        if active {
            fill_diffs(&mut data).await;
        }
        groups.push(BoardGroup { batch, data, active });
    }
    // Stable: active first, each part keeping load_batches' order (solo, then newest).
    groups.sort_by_key(|g| !g.active);
    Ok(groups)
}

/// How many worktrees get their diff computed at once (3 git processes each).
const DIFF_CONCURRENCY: usize = 4;

/// Diffs of the items still open (todo neither done nor discarded, worktree
/// alive), a few worktrees at a time.
async fn fill_diffs(data: &mut BoardData) {
    let open: Vec<usize> = data
        .items
        .iter()
        .enumerate()
        .filter(|(_, i)| !i.todo.as_ref().is_some_and(|t| t.done || t.status == "todo"))
        .filter(|(_, i)| i.worktree.as_ref().is_some_and(|w| w.removed_at.is_none() && Path::new(&w.path).is_dir()))
        .map(|(n, _)| n)
        .collect();
    for chunk in open.chunks(DIFF_CONCURRENCY) {
        let stats = futures_util::future::join_all(
            chunk.iter().map(|&n| diff_stat(data.items[n].worktree.as_ref().expect("filtered"))),
        )
        .await;
        for (&n, d) in chunk.iter().zip(stats) {
            data.items[n].diff = Some(d);
        }
    }
}

fn group_active(data: &BoardData) -> bool {
    let busy = |s: &str| s == "running" || s == "fetched";
    data.items.iter().any(|i| {
        busy(&i.status)
            || i.todo.as_ref().is_some_and(|t| !t.done && (t.status == "doing" || t.status == "waiting"))
    })
}

async fn worktree_by_path(db: &Db, path: &str) -> Option<worktrees::Worktree> {
    let id: String = sqlx::query_scalar("SELECT id FROM worktrees WHERE path = ?")
        .bind(path)
        .fetch_optional(db)
        .await
        .ok()??;
    worktrees::get(db, &id).await.ok()
}

async fn diff_stat(w: &worktrees::Worktree) -> DiffStat {
    let dir = Path::new(&w.path);
    let range = format!("{}...HEAD", w.base_ref);
    let since = format!("{}..HEAD", w.base_ref);
    let count_args = ["rev-list", "--count", since.as_str()];
    let short_args = ["diff", "--shortstat", range.as_str()];
    let (count, short, status) = tokio::join!(
        worktrees::git_local(dir, &count_args),
        worktrees::git_local(dir, &short_args),
        worktrees::git_local(dir, &["status", "--porcelain"]),
    );
    let commits = count.ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    let (files, insertions, deletions) = short.map(|s| parse_shortstat(&s)).unwrap_or((0, 0, 0));
    let uncommitted = status
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count() as i64)
        .unwrap_or(0);
    DiffStat { commits, files, insertions, deletions, uncommitted }
}

/// `3 files changed, 10 insertions(+), 2 deletions(-)` → (3, 10, 2).
fn parse_shortstat(s: &str) -> (i64, i64, i64) {
    let mut out = (0, 0, 0);
    for part in s.split(',') {
        let part = part.trim();
        let n: i64 = part.split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(0);
        if part.contains("file") {
            out.0 = n;
        } else if part.contains("insertion") {
            out.1 = n;
        } else if part.contains("deletion") {
            out.2 = n;
        }
    }
    out
}

/// Commit everything in a worktree (as the project's git identity).
#[tauri::command]
pub async fn worktree_commit_all(
    app: AppHandle,
    db: State<'_, Db>,
    id: String,
    message: String,
) -> Result<(), String> {
    worktrees::commit_all(&app, db.inner(), &id, &message).await?;
    let _ = app.emit(BOARD_CHANGED_EVENT, ());
    Ok(())
}

/// Push the worktree's branch and return the page that opens a PR / MR for it.
#[tauri::command]
pub async fn worktree_push(app: AppHandle, db: State<'_, Db>, id: String) -> Result<String, String> {
    let url = worktrees::push(&app, db.inner(), &id).await?;
    let _ = app.emit(BOARD_CHANGED_EVENT, ());
    Ok(url)
}

/// Close a board item: `done` marks its todo done (after review / PR), else the
/// todo goes back to `todo` (work discarded). The worktree is removed; its branch
/// is kept when `done` (it may back a PR) and deleted otherwise.
#[tauri::command]
pub async fn board_close_item(
    app: AppHandle,
    db: State<'_, Db>,
    run_id: String,
    done: bool,
) -> Result<(), String> {
    let db = db.inner();
    let row = sqlx::query(
        "SELECT r.status, r.worktree_path,
                (SELECT ir.todo_id FROM todo_runs ir WHERE ir.run_id = r.id AND ir.source = 'created_from_item' ORDER BY ir.linked_at LIMIT 1) AS todo_id
         FROM runs r WHERE r.id = ?",
    )
    .bind(&run_id)
    .fetch_one(db)
    .await
    .map_err(|e| e.to_string())?;
    if row.get::<String, _>("status") == "running" {
        return Err("worktree_busy: the session is still running — cancel it first".into());
    }
    if let Some(path) = row.get::<Option<String>, _>("worktree_path") {
        if let Some(wt) = worktree_by_path(db, &path).await.filter(|w| w.removed_at.is_none()) {
            worktrees::remove(db, &wt.id, !done).await?;
        }
    }
    if let Some(todo_id) = row.get::<Option<String>, _>("todo_id") {
        let now = chrono::Utc::now().to_rfc3339();
        let q = if done {
            sqlx::query("UPDATE todos SET done = 1, completed_at = COALESCE(completed_at, ?), updated_at = ? WHERE id = ?")
                .bind(&now)
                .bind(&now)
        } else {
            sqlx::query("UPDATE todos SET status = 'todo', updated_at = ? WHERE id = ? AND done = 0").bind(&now)
        };
        q.bind(&todo_id).execute(db).await.map_err(|e| e.to_string())?;
        crate::focus::emit_todos_changed(&app);
    }
    let _ = app.emit(BOARD_CHANGED_EVENT, ());
    Ok(())
}

/// Every pending permission / question across all runs (for the board's
/// "needs you" lane when a run's own listeners missed it).
#[tauri::command]
pub async fn list_all_pending_permissions() -> Result<Vec<crate::runs::permission::PermissionRequestEvent>, String> {
    Ok(crate::runs::permission::all_pending().into_iter().map(|p| p.event).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn todo_sessions_outside_a_batch_show_as_the_solo_batch() {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&db).await.unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        for sql in [
            "INSERT INTO projects (id, name, path, created_at) VALUES ('p', 'shop', '/tmp/p', ?1)",
            "INSERT INTO todos (id, text, done, position, created_at, project_id) VALUES ('t', 'Fix login', 0, 0, ?1, 'p')",
            // Started on its own ("Start with AI"), linked to the todo.
            "INSERT INTO runs (id, project_id, type, status, created_at) VALUES ('solo1', 'p', 'session', 'done', ?1)",
            "INSERT INTO todo_runs (todo_id, run_id, project_id, source, linked_at) VALUES ('t', 'solo1', 'p', 'created_from_item', ?1)",
            // A plain session (no todo) and a batch conductor stay out of it.
            "INSERT INTO runs (id, project_id, type, status, created_at) VALUES ('plain', 'p', 'session', 'done', ?1)",
            "INSERT INTO runs (id, project_id, type, status, created_at, todo_batch) VALUES ('b', 'p', 'session', 'running', ?1, 1)",
            "INSERT INTO todo_runs (todo_id, run_id, project_id, source, linked_at) VALUES ('t', 'b', 'p', 'created_from_item', ?1)",
        ] {
            sqlx::query(sql).bind(&now).execute(&db).await.unwrap();
        }

        let batches = load_batches(&db).await.unwrap();
        assert_eq!(batches.iter().map(|b| b.run_id.as_str()).collect::<Vec<_>>(), [SOLO_BATCH, "b"]);
        assert_eq!(batches[0].worker_count, 1);

        let board = load_board(&db, SOLO_BATCH).await.unwrap();
        assert_eq!(board.items.len(), 1);
        assert_eq!(board.items[0].run_id, "solo1");
        assert_eq!(board.items[0].role, "worker");
        assert_eq!(board.items[0].todo.as_ref().map(|t| t.text.as_str()), Some("Fix login"));
        assert!(board.queued.is_empty());
    }

    #[tokio::test]
    async fn the_queue_lists_open_todos_in_added_order() {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&db).await.unwrap();
        for sql in [
            "INSERT INTO projects (id, name, path, created_at) VALUES ('p', 'shop', '/tmp/p', 'c')",
            "INSERT INTO todos (id, text, done, position, created_at, project_id) VALUES ('a', 'A', 0, 0, 'c', 'p')",
            "INSERT INTO todos (id, text, done, position, created_at, project_id) VALUES ('b', 'B', 0, 1, 'c', 'p')",
            "INSERT INTO todos (id, text, done, position, created_at, project_id) VALUES ('d', 'Done', 1, 2, 'c', 'p')",
            "INSERT INTO board_queue (todo_id, added_at) VALUES ('b', '2026-01-01T00:00:01Z')",
            "INSERT INTO board_queue (todo_id, added_at) VALUES ('a', '2026-01-01T00:00:02Z')",
            "INSERT INTO board_queue (todo_id, added_at) VALUES ('d', '2026-01-01T00:00:03Z')",
        ] {
            sqlx::query(sql).execute(&db).await.unwrap();
        }
        let q = load_queue(&db).await.unwrap();
        assert_eq!(q.iter().map(|x| x.todo.id.as_str()).collect::<Vec<_>>(), ["b", "a"]);
        assert_eq!(q[0].project_name.as_deref(), Some("shop"));
        // Deleting a todo takes it out of the queue too.
        sqlx::query("DELETE FROM todos WHERE id = 'b'").execute(&db).await.unwrap();
        assert_eq!(load_queue(&db).await.unwrap().len(), 1);
    }

    async fn mem_db() -> Db {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&db).await.unwrap();
        db
    }

    #[tokio::test]
    async fn queued_todos_split_by_project_and_busy_ones_are_skipped() {
        let db = mem_db().await;
        for sql in [
            "INSERT INTO projects (id, name, path, created_at) VALUES ('p1', 'shop', '/tmp/p1', 'c')",
            "INSERT INTO projects (id, name, path, created_at) VALUES ('p2', 'admin', '/tmp/p2', 'c')",
            "INSERT INTO todos (id, text, done, position, created_at, project_id) VALUES ('a', 'A', 0, 0, 'c', 'p1')",
            "INSERT INTO todos (id, text, done, position, created_at, project_id) VALUES ('b', 'B', 0, 1, 'c', 'p2')",
            "INSERT INTO todos (id, text, done, position, created_at, project_id) VALUES ('c', 'C', 0, 2, 'c', 'p1')",
            "INSERT INTO todos (id, text, done, position, created_at, project_id) VALUES ('d', 'Done', 1, 3, 'c', 'p1')",
            "INSERT INTO todos (id, text, done, position, created_at) VALUES ('n', 'No project', 0, 4, 'c')",
            // `w` waits for review in an earlier batch; `r` has a session running.
            "INSERT INTO todos (id, text, done, position, created_at, project_id, status) VALUES ('w', 'W', 0, 5, 'c', 'p1', 'waiting')",
            "INSERT INTO todos (id, text, done, position, created_at, project_id) VALUES ('r', 'R', 0, 6, 'c', 'p2')",
            "INSERT INTO runs (id, project_id, type, status, created_at, todo_batch) VALUES ('old', 'p1', 'session', 'done', 'c', 1)",
            "INSERT INTO todo_batch_items (batch_run_id, todo_id, position) VALUES ('old', 'w', 0)",
            "INSERT INTO runs (id, project_id, type, status, created_at) VALUES ('live', 'p2', 'session', 'running', 'c')",
            "INSERT INTO todo_runs (todo_id, run_id, project_id, source, linked_at) VALUES ('r', 'live', 'p2', 'created_from_item', 'c')",
        ] {
            sqlx::query(sql).execute(&db).await.unwrap();
        }
        let ids: Vec<String> = ["a", "b", "c", "d", "n", "w", "r", "zz", "a"].iter().map(|s| s.to_string()).collect();
        let (ok, skipped) = runnable_todos(&db, &ids).await.unwrap();
        let reasons: Vec<(&str, &str)> = skipped.iter().map(|s| (s.id.as_str(), s.reason.as_str())).collect();
        assert_eq!(reasons, [("d", "done"), ("n", "no_project"), ("w", "in_progress"), ("r", "in_progress"), ("zz", "not_found")]);
        let groups = group_by_project(ok);
        let shape: Vec<(&str, Vec<&str>)> =
            groups.iter().map(|(p, ts)| (p.as_str(), ts.iter().map(|t| t.id.as_str()).collect())).collect();
        assert_eq!(shape, [("p1", vec!["a", "c"]), ("p2", vec!["b"])]);
    }

    #[tokio::test]
    async fn finished_batches_move_to_history() {
        let db = mem_db().await;
        for sql in [
            "INSERT INTO projects (id, name, path, created_at) VALUES ('p', 'shop', '/tmp/p', 'c')",
            "INSERT INTO todos (id, text, done, position, created_at, project_id, status) VALUES ('t1', 'A', 0, 0, 'c', 'p', 'waiting')",
            "INSERT INTO todos (id, text, done, position, created_at, project_id) VALUES ('t2', 'B', 1, 1, 'c', 'p')",
            // Older batch: its worker waits for review → still active.
            "INSERT INTO runs (id, project_id, type, status, created_at, todo_batch) VALUES ('b1', 'p', 'session', 'done', '2026-01-01', 1)",
            "INSERT INTO runs (id, project_id, type, status, created_at, role, conductor_run_id) VALUES ('w1', 'p', 'session', 'done', '2026-01-01', 'worker', 'b1')",
            "INSERT INTO todo_runs (todo_id, run_id, project_id, source, linked_at) VALUES ('t1', 'w1', 'p', 'created_from_item', 'c')",
            // Newer batch: everything done → history.
            "INSERT INTO runs (id, project_id, type, status, created_at, todo_batch) VALUES ('b2', 'p', 'session', 'done', '2026-02-01', 1)",
            "INSERT INTO runs (id, project_id, type, status, created_at, role, conductor_run_id) VALUES ('w2', 'p', 'session', 'done', '2026-02-01', 'worker', 'b2')",
            "INSERT INTO todo_runs (todo_id, run_id, project_id, source, linked_at) VALUES ('t2', 'w2', 'p', 'created_from_item', 'c')",
        ] {
            sqlx::query(sql).execute(&db).await.unwrap();
        }
        let groups = load_overview(&db).await.unwrap();
        let shape: Vec<(&str, bool)> = groups.iter().map(|g| (g.batch.run_id.as_str(), g.active)).collect();
        assert_eq!(shape, [("b1", true), ("b2", false)]);
        assert_eq!(groups[0].data.items.len(), 2);
    }

    #[tokio::test]
    async fn a_todo_waiting_in_a_live_batch_is_not_run_twice() {
        let db = mem_db().await;
        for sql in [
            "INSERT INTO projects (id, name, path, created_at) VALUES ('p', 'shop', '/tmp/p', 'c')",
            "INSERT INTO todos (id, text, done, position, created_at, project_id) VALUES ('q', 'Q', 0, 0, 'c', 'p')",
            // Batch idle between turns, but one of its workers still runs.
            "INSERT INTO runs (id, project_id, type, status, created_at, todo_batch) VALUES ('b', 'p', 'session', 'done', 'c', 1)",
            "INSERT INTO runs (id, project_id, type, status, created_at, role, conductor_run_id) VALUES ('w', 'p', 'session', 'running', 'c', 'worker', 'b')",
            "INSERT INTO todo_batch_items (batch_run_id, todo_id, position) VALUES ('b', 'q', 0)",
        ] {
            sqlx::query(sql).execute(&db).await.unwrap();
        }
        assert!(in_progress(&db, "q").await.unwrap());
        // The batch is over (nothing running): the todo can be run again.
        sqlx::query("UPDATE runs SET status = 'done' WHERE id = 'w'").execute(&db).await.unwrap();
        assert!(!in_progress(&db, "q").await.unwrap());
    }

    #[tokio::test]
    async fn an_old_open_batch_stays_on_the_board() {
        let db = mem_db().await;
        sqlx::query("INSERT INTO projects (id, name, path, created_at) VALUES ('p', 'shop', '/tmp/p', 'c')").execute(&db).await.unwrap();
        // The oldest batch still runs; 30 newer finished ones follow it.
        sqlx::query("INSERT INTO runs (id, project_id, type, status, created_at, todo_batch) VALUES ('old', 'p', 'session', 'running', '2026-01-01', 1)")
            .execute(&db).await.unwrap();
        for i in 0..30 {
            sqlx::query("INSERT INTO runs (id, project_id, type, status, created_at, todo_batch) VALUES (?, 'p', 'session', 'done', ?, 1)")
                .bind(format!("n{i}"))
                .bind(format!("2026-02-{:02}", i % 28 + 1))
                .execute(&db).await.unwrap();
        }
        assert!(load_batches(&db).await.unwrap().iter().any(|b| b.run_id == "old"));
        let groups = load_overview(&db).await.unwrap();
        assert_eq!(groups.len(), 31);
        assert_eq!((groups[0].batch.run_id.as_str(), groups[0].active), ("old", true));
    }

    #[tokio::test]
    async fn a_worker_whose_todo_moved_project_cant_resume() {
        let db = mem_db().await;
        for sql in [
            "INSERT INTO projects (id, name, path, created_at) VALUES ('p1', 'shop', '/tmp/p1', 'c')",
            "INSERT INTO projects (id, name, path, created_at) VALUES ('p2', 'admin', '/tmp/p2', 'c')",
            "INSERT INTO todos (id, text, done, position, created_at, project_id) VALUES ('t', 'T', 0, 0, 'c', 'p1')",
            "INSERT INTO runs (id, project_id, type, status, created_at) VALUES ('w', 'p1', 'session', 'done', 'c')",
            "INSERT INTO todo_runs (todo_id, run_id, project_id, source, linked_at) VALUES ('t', 'w', 'p1', 'created_from_item', 'c')",
        ] {
            sqlx::query(sql).execute(&db).await.unwrap();
        }
        use crate::conductor::tools::ensure_worker_in_todo_project as check;
        assert!(check(&db, "w").await.is_ok());
        sqlx::query("UPDATE todos SET project_id = 'p2' WHERE id = 't'").execute(&db).await.unwrap();
        assert!(check(&db, "w").await.unwrap_err().starts_with("wrong_project"));
    }

    #[tokio::test]
    async fn a_todo_being_worked_on_cant_change_project() {
        let db = mem_db().await;
        for sql in [
            "INSERT INTO projects (id, name, path, created_at) VALUES ('p1', 'shop', '/tmp/p1', 'c')",
            "INSERT INTO projects (id, name, path, created_at) VALUES ('p2', 'admin', '/tmp/p2', 'c')",
            "INSERT INTO todos (id, text, done, position, created_at, project_id) VALUES ('t', 'T', 0, 0, 'c', 'p1')",
            // Queued in a live batch, no worker yet.
            "INSERT INTO runs (id, project_id, type, status, created_at, todo_batch) VALUES ('b', 'p1', 'session', 'running', 'c', 1)",
            "INSERT INTO todo_batch_items (batch_run_id, todo_id, position) VALUES ('b', 't', 0)",
        ] {
            sqlx::query(sql).execute(&db).await.unwrap();
        }
        use crate::commands::todos::set_todo_project_inner as set;
        assert!(set(&db, "t", Some("p2")).await.unwrap_err().starts_with("todo_busy"));
        assert!(set(&db, "t", None).await.unwrap_err().starts_with("todo_busy"));
        // Its own project again is not a move.
        set(&db, "t", Some("p1")).await.unwrap();
        assert!(set(&db, "nope", Some("p2")).await.is_err());
        sqlx::query("UPDATE runs SET status = 'done' WHERE id = 'b'").execute(&db).await.unwrap();
        set(&db, "t", Some("p2")).await.unwrap();
        assert_eq!(get_todo(&db, "t").await.unwrap().project_id.as_deref(), Some("p2"));
    }

    #[test]
    fn shortstat_parses_every_part() {
        assert_eq!(parse_shortstat(" 3 files changed, 10 insertions(+), 2 deletions(-)"), (3, 10, 2));
        assert_eq!(parse_shortstat(" 1 file changed, 1 deletion(-)"), (1, 0, 1));
        assert_eq!(parse_shortstat(""), (0, 0, 0));
    }

}
