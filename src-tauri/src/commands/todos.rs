use crate::db::Db;
use serde::Serialize;
use sqlx::Row;
use tauri::State;
use uuid::Uuid;

/// A single quick-note todo. `position` orders the list (ascending: lower value
/// sits higher / higher priority). Drag-and-drop reorder rewrites positions.
///
/// `project_id` / `run_id` are the capture context: set when the todo was jotted
/// down from inside a project's run workspace, so the list can filter by project
/// and link back to the conversation it came from. Both are nullable and
/// unenforced — a globally captured todo has neither.
#[derive(Debug, Serialize, Clone)]
pub struct Todo {
    pub id: String,
    pub text: String,
    pub done: bool,
    pub position: i64,
    pub created_at: String,
    pub project_id: Option<String>,
    pub run_id: Option<String>,
}

fn row_to_todo(row: &sqlx::sqlite::SqliteRow) -> Todo {
    Todo {
        id: row.get("id"),
        text: row.get("text"),
        done: row.get("done"),
        position: row.get("position"),
        created_at: row.get("created_at"),
        project_id: row.get("project_id"),
        run_id: row.get("run_id"),
    }
}

#[tauri::command]
pub async fn list_todos(db: State<'_, Db>) -> Result<Vec<Todo>, String> {
    let rows = sqlx::query(
        "SELECT id, text, done, position, created_at, project_id, run_id FROM todos \
         ORDER BY position ASC, created_at DESC",
    )
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows.iter().map(row_to_todo).collect())
}

#[tauri::command]
pub async fn add_todo(
    db: State<'_, Db>,
    text: String,
    project_id: Option<String>,
    run_id: Option<String>,
) -> Result<Todo, String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("text is required".into());
    }
    let id = Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    // New todos go to the top (highest priority) by taking a position below the
    // current minimum.
    let min_pos: Option<i64> = sqlx::query("SELECT MIN(position) AS min_pos FROM todos")
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?
        .get("min_pos");
    let position = min_pos.unwrap_or(0) - 1;

    sqlx::query(
        "INSERT INTO todos (id, text, done, position, created_at, project_id, run_id) \
         VALUES (?, ?, 0, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&text)
    .bind(position)
    .bind(&created_at)
    .bind(&project_id)
    .bind(&run_id)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(Todo {
        id,
        text,
        done: false,
        position,
        created_at,
        project_id,
        run_id,
    })
}

/// Link or unlink a todo from a project. Pass `None` to clear the link; the run
/// backlink is cleared with it, since a run only makes sense inside its project.
#[tauri::command]
pub async fn set_todo_project(
    db: State<'_, Db>,
    id: String,
    project_id: Option<String>,
) -> Result<(), String> {
    if project_id.is_none() {
        sqlx::query("UPDATE todos SET project_id = NULL, run_id = NULL WHERE id = ?")
            .bind(&id)
            .execute(db.inner())
            .await
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    sqlx::query("UPDATE todos SET project_id = ? WHERE id = ?")
        .bind(&project_id)
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn toggle_todo(db: State<'_, Db>, id: String) -> Result<(), String> {
    sqlx::query("UPDATE todos SET done = 1 - done WHERE id = ?")
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn update_todo(db: State<'_, Db>, id: String, text: String) -> Result<(), String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("text is required".into());
    }
    sqlx::query("UPDATE todos SET text = ? WHERE id = ?")
        .bind(&text)
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn delete_todo(db: State<'_, Db>, id: String) -> Result<(), String> {
    sqlx::query("DELETE FROM todos WHERE id = ?")
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Bulk-delete todos by id in a single transaction, so a failure mid-way rolls
/// back the whole batch instead of leaving a partial deletion.
#[tauri::command]
pub async fn delete_todos(db: State<'_, Db>, ids: Vec<String>) -> Result<(), String> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut tx = db.inner().begin().await.map_err(|e| e.to_string())?;
    for id in &ids {
        sqlx::query("DELETE FROM todos WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
    }
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn clear_completed_todos(db: State<'_, Db>) -> Result<(), String> {
    sqlx::query("DELETE FROM todos WHERE done = 1")
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Persist a new order. The frontend sends the full list of ids top-to-bottom;
/// each row's position becomes its index, so the ordering is stable regardless
/// of prior position values. Done in one transaction to avoid partial reorders.
#[tauri::command]
pub async fn reorder_todos(db: State<'_, Db>, ids: Vec<String>) -> Result<(), String> {
    let mut tx = db.inner().begin().await.map_err(|e| e.to_string())?;
    for (index, id) in ids.iter().enumerate() {
        sqlx::query("UPDATE todos SET position = ? WHERE id = ?")
            .bind(index as i64)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
    }
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(())
}
