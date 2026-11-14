//! Todo batches: hand a set of todos (across projects) to one Todo Conductor
//! session, and the data + actions behind the Batch Board that tracks it.
//!
//! The conductor is an ordinary top-level session (every one is a conductor)
//! flagged `runs.todo_batch = 1`; its workers run in the todos' projects and keep
//! their changes apart as the batch's isolation says: following each project's
//! own worktree rules (default), in Devdy worktrees, or one at a time.

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;
use sqlx::Row;
use tauri::{AppHandle, Emitter, State};

use crate::commands::runs::{create_session_run_inner, start_run_inner, StartRunPayload};
use crate::commands::todos::get_todo;
use crate::db::Db;
use crate::focus::ai::{batch_prompt, BatchIsolation, BatchOptions, BatchTodo};
use crate::runs::RunRegistry;
use crate::worktrees;

/// Emitted after a board action changed a worktree or a todo.
pub const BOARD_CHANGED_EVENT: &str = "board:changed";

/// Start a Todo Conductor on `todo_ids`; returns its run id. Errors
/// `project_required: <ids>` when some todos have no project. The conductor
/// lives in `home_project_id`, else the project most of the todos belong to.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn todo_batch_start(
    app: AppHandle,
    db: State<'_, Db>,
    registry: State<'_, RunRegistry>,
    todo_ids: Vec<String>,
    home_project_id: Option<String>,
    max_parallel: Option<usize>,
    // `rules` (default: workers follow their project's worktree rules), `devdy`
    // (Devdy creates a worktree per worker) or `none`.
    isolation: Option<String>,
    engine: Option<String>,
    permission_mode: Option<String>,
) -> Result<String, String> {
    let db = db.inner().clone();
    if todo_ids.is_empty() {
        return Err("no todos selected".into());
    }
    let mut todos = Vec::new();
    for id in &todo_ids {
        let t = get_todo(&db, id).await?;
        if !t.done {
            todos.push(t);
        }
    }
    if todos.is_empty() {
        return Err("every selected todo is already done".into());
    }
    let missing: Vec<&str> = todos.iter().filter(|t| t.project_id.is_none()).map(|t| t.id.as_str()).collect();
    if !missing.is_empty() {
        return Err(format!("project_required: {}", missing.join(",")));
    }

    let names = project_names(&db).await?;
    let home = match home_project_id.filter(|p| names.contains_key(p)) {
        Some(p) => p,
        None => most_common_project(todos.iter().filter_map(|t| t.project_id.as_deref())).unwrap_or_default(),
    };
    let max_parallel = max_parallel.unwrap_or(3).clamp(1, crate::conductor::DEFAULT_MAX_WORKERS);
    let isolation = BatchIsolation::parse(isolation.as_deref());
    let batch: Vec<BatchTodo> = todos
        .iter()
        .map(|t| BatchTodo {
            id: &t.id,
            text: &t.text,
            project: t.project_id.as_deref().and_then(|p| names.get(p)).map(String::as_str).unwrap_or("?"),
            source_url: t.source_url.as_deref(),
        })
        .collect();
    let prompt = batch_prompt(
        &batch,
        &BatchOptions {
            max_parallel,
            isolation,
            engine: engine.as_deref().filter(|e| !e.is_empty()),
            permission_mode: permission_mode.as_deref().filter(|m| !m.is_empty()),
        },
    );

    let run = create_session_run_inner(&db, home, None).await?;
    let title = format!("🗂 Todo batch ({})", todos.len());
    sqlx::query("UPDATE runs SET title = ?, todo_batch = 1 WHERE id = ?")
        .bind(&title)
        .bind(&run.id)
        .execute(&db)
        .await
        .map_err(|e| e.to_string())?;
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
    for (i, t) in todos.iter().enumerate() {
        sqlx::query("INSERT OR IGNORE INTO todo_batch_items (batch_run_id, todo_id, position) VALUES (?, ?, ?)")
            .bind(&run.id)
            .bind(&t.id)
            .bind(i as i64)
            .execute(&db)
            .await
            .map_err(|e| e.to_string())?;
    }
    if let Err(e) = start_run_inner(app.clone(), db.clone(), registry.inner().clone(), payload).await {
        let _ = sqlx::query("DELETE FROM conductor_sessions WHERE id = ?").bind(&run.id).execute(&db).await;
        let _ = sqlx::query("DELETE FROM todo_batch_items WHERE batch_run_id = ?").bind(&run.id).execute(&db).await;
        let _ = sqlx::query("DELETE FROM runs WHERE id = ?").bind(&run.id).execute(&db).await;
        return Err(e);
    }
    // Started: these todos leave the board queue.
    for t in &todos {
        let _ = sqlx::query("DELETE FROM board_queue WHERE todo_id = ?").bind(&t.id).execute(&db).await;
    }
    let _ = app.emit(BOARD_CHANGED_EVENT, ());
    Ok(run.id)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedTodo {
    pub todo: BoardTodo,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub added_at: String,
}

/// "Add to Batch Board": queue a todo to be run from the board. Errors
/// `project_required` for a todo without a project (its session needs one).
#[tauri::command]
pub async fn board_queue_add(app: AppHandle, db: State<'_, Db>, todo_id: String) -> Result<(), String> {
    let todo = get_todo(db.inner(), &todo_id).await?;
    if todo.project_id.is_none() {
        return Err("project_required: pick a project for this todo first".into());
    }
    sqlx::query("INSERT OR IGNORE INTO board_queue (todo_id, added_at) VALUES (?, ?)")
        .bind(&todo_id)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    let _ = app.emit(BOARD_CHANGED_EVENT, ());
    Ok(())
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

/// The id seen most often (first seen wins a tie).
fn most_common_project<'a>(ids: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for id in ids {
        match counts.iter_mut().find(|(k, _)| *k == id) {
            Some((_, n)) => *n += 1,
            None => counts.push((id, 1)),
        }
    }
    let best = counts.iter().map(|(_, n)| *n).max()?;
    counts.iter().find(|(_, n)| *n == best).map(|(k, _)| k.to_string())
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
    let rows = sqlx::query(
        "SELECT r.id, r.title, r.project_id, p.name AS project_name, r.status, r.created_at,
                (SELECT COUNT(*) FROM runs w WHERE w.conductor_run_id = r.id) AS worker_count
         FROM runs r JOIN projects p ON p.id = r.project_id
         WHERE r.todo_batch = 1 ORDER BY r.created_at DESC LIMIT 30",
    )
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
            Some(w) if w.removed_at.is_none() && Path::new(&w.path).is_dir() => Some(diff_stat(w).await),
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
    let commits = worktrees::git_local(dir, &["rev-list", "--count", &format!("{}..HEAD", w.base_ref)])
        .await
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let (files, insertions, deletions) = worktrees::git_local(dir, &["diff", "--shortstat", &range])
        .await
        .map(|s| parse_shortstat(&s))
        .unwrap_or((0, 0, 0));
    let uncommitted = worktrees::git_local(dir, &["status", "--porcelain"])
        .await
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

    #[test]
    fn shortstat_parses_every_part() {
        assert_eq!(parse_shortstat(" 3 files changed, 10 insertions(+), 2 deletions(-)"), (3, 10, 2));
        assert_eq!(parse_shortstat(" 1 file changed, 1 deletion(-)"), (1, 0, 1));
        assert_eq!(parse_shortstat(""), (0, 0, 0));
    }

    #[test]
    fn most_common_project_prefers_count_then_first() {
        assert_eq!(most_common_project(["a", "b", "b"].into_iter()), Some("b".into()));
        assert_eq!(most_common_project(["a", "b"].into_iter()), Some("a".into()));
        assert_eq!(most_common_project(std::iter::empty()), None);
    }
}
