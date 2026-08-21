use crate::db::Db;
use serde::Serialize;
use sqlx::Row;
use tauri::State;
use uuid::Uuid;

/// A single quick-note todo. `position` orders the list (ascending: lower value
/// sits higher / higher priority). Drag-and-drop reorder rewrites positions.
#[derive(Debug, Serialize, Clone)]
pub struct Todo {
    pub id: String,
    pub text: String,
    pub done: bool,
    pub position: i64,
    pub created_at: String,
}

fn row_to_todo(row: &sqlx::sqlite::SqliteRow) -> Todo {
    Todo {
        id: row.get("id"),
        text: row.get("text"),
        done: row.get("done"),
        position: row.get("position"),
        created_at: row.get("created_at"),
    }
}

#[tauri::command]
pub async fn list_todos(db: State<'_, Db>) -> Result<Vec<Todo>, String> {
    let rows = sqlx::query(
        "SELECT id, text, done, position, created_at FROM todos \
         ORDER BY position ASC, created_at DESC",
    )
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows.iter().map(row_to_todo).collect())
}

#[tauri::command]
pub async fn add_todo(db: State<'_, Db>, text: String) -> Result<Todo, String> {
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
        "INSERT INTO todos (id, text, done, position, created_at) VALUES (?, ?, 0, ?, ?)",
    )
    .bind(&id)
    .bind(&text)
    .bind(position)
    .bind(&created_at)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(Todo {
        id,
        text,
        done: false,
        position,
        created_at,
    })
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
