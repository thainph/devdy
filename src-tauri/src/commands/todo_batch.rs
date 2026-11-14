//! Todo batches: hand a set of todos (across projects) to one Todo Conductor
//! session, and the data + actions behind the Batch Board that tracks it.
//!
//! The conductor is an ordinary top-level session (every one is a conductor)
//! flagged `runs.todo_batch = 1`; its workers run in the todos' projects, usually
//! in their own worktrees (`session_spawn` with `todo_id` / `worktree`).

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;
use sqlx::Row;
use tauri::{AppHandle, Emitter, State};

use crate::commands::runs::{create_session_run_inner, start_run_inner, StartRunPayload};
use crate::commands::todos::get_todo;
use crate::db::Db;
use crate::focus::ai::{batch_prompt, BatchOptions, BatchTodo};
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
    worktree: Option<bool>,
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
    let worktree = worktree.unwrap_or(true);
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
            worktree,
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
    if let Err(e) = start_run_inner(app.clone(), db.clone(), registry.inner().clone(), payload).await {
        let _ = sqlx::query("DELETE FROM conductor_sessions WHERE id = ?").bind(&run.id).execute(&db).await;
        let _ = sqlx::query("DELETE FROM runs WHERE id = ?").bind(&run.id).execute(&db).await;
        return Err(e);
    }
    let _ = app.emit(BOARD_CHANGED_EVENT, ());
    Ok(run.id)
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
    let rows = sqlx::query(
        "SELECT r.id, r.title, r.project_id, p.name AS project_name, r.status, r.created_at,
                (SELECT COUNT(*) FROM runs w WHERE w.conductor_run_id = r.id) AS worker_count
         FROM runs r JOIN projects p ON p.id = r.project_id
         WHERE r.todo_batch = 1 ORDER BY r.created_at DESC LIMIT 30",
    )
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows
        .iter()
        .map(|r| BatchSummary {
            run_id: r.get("id"),
            title: r.get("title"),
            project_id: r.get("project_id"),
            project_name: r.get("project_name"),
            status: r.get("status"),
            created_at: r.get("created_at"),
            worker_count: r.get("worker_count"),
        })
        .collect())
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

/// The batch's conductor and every worker it spawned, with todo, worktree and
/// diff. Live state (permission prompts, streaming) comes from the run events.
#[tauri::command]
pub async fn board_items(db: State<'_, Db>, batch_run_id: String) -> Result<Vec<BoardItem>, String> {
    let db = db.inner();
    let rows = sqlx::query(
        "SELECT r.id, r.title, r.role, r.project_id, p.name AS project_name, r.engine, r.status,
                r.created_at, r.started_at, r.finished_at, r.worktree_path,
                (SELECT COALESCE(SUM(u.cost_usd), 0) FROM run_usage u WHERE u.run_id = r.id) AS cost,
                (SELECT ir.todo_id FROM todo_runs ir WHERE ir.run_id = r.id ORDER BY ir.linked_at LIMIT 1) AS todo_id
         FROM runs r JOIN projects p ON p.id = r.project_id
         WHERE r.id = ?1 OR r.conductor_run_id = ?1
         ORDER BY r.id != ?1, r.created_at",
    )
    .bind(&batch_run_id)
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())?;

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
            cost_usd: r.get("cost"),
            todo,
            worktree: wt,
            diff,
        });
    }
    Ok(items)
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
                (SELECT ir.todo_id FROM todo_runs ir WHERE ir.run_id = r.id ORDER BY ir.linked_at LIMIT 1) AS todo_id
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
