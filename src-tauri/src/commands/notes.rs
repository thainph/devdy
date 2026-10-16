use crate::db::Db;
use crate::item_links::{self, ItemKind};
use serde::Serialize;
use sqlx::Row;
use tauri::State;
use uuid::Uuid;

/// A single quick note. `title` is a short label (may be empty — the UI falls
/// back to the first line of `content`), `content` is markdown. `project_id`
/// optionally links the note to a project. Sessions (runs) are linked through
/// `note_runs` (see `item_links`); `linked_run_count` counts those whose run
/// still exists. `position` orders the list (ascending: lower value sits
/// higher / top). Drag-and-drop reorder rewrites positions.
#[derive(Debug, Serialize, Clone)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub content: String,
    pub project_id: Option<String>,
    pub position: i64,
    pub created_at: String,
    pub updated_at: String,
    pub linked_run_count: i64,
}

fn row_to_note(row: &sqlx::sqlite::SqliteRow) -> Note {
    Note {
        id: row.get("id"),
        title: row.get("title"),
        content: row.get("content"),
        project_id: row.get("project_id"),
        position: row.get("position"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        linked_run_count: row.get("linked_run_count"),
    }
}

/// List notes, newest-priority first. When `project_id` is `Some`, only notes
/// linked to that project are returned; otherwise all notes are returned.
#[tauri::command]
pub async fn list_notes(
    db: State<'_, Db>,
    project_id: Option<String>,
) -> Result<Vec<Note>, String> {
    let mut sql = format!(
        "SELECT n.id, n.title, n.content, n.project_id, n.position, n.created_at, n.updated_at, \
         {} FROM notes n",
        item_links::linked_count_sql(ItemKind::Note, "n")
    );
    if project_id.is_some() {
        sql.push_str(" WHERE n.project_id = ?");
    }
    sql.push_str(" ORDER BY n.position ASC, n.updated_at DESC");
    let mut q = sqlx::query(&sql);
    if let Some(pid) = project_id {
        q = q.bind(pid);
    }
    let rows = q.fetch_all(db.inner()).await.map_err(|e| e.to_string())?;
    Ok(rows.iter().map(row_to_note).collect())
}

#[tauri::command]
pub async fn add_note(
    db: State<'_, Db>,
    title: String,
    content: String,
    project_id: Option<String>,
    run_id: Option<String>,
) -> Result<Note, String> {
    let title = title.trim().to_string();
    let content = content.trim().to_string();
    if title.is_empty() && content.is_empty() {
        return Err("a title or content is required".into());
    }
    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    // New notes go to the top (highest priority) by taking a position below the
    // current minimum.
    let min_pos: Option<i64> = sqlx::query("SELECT MIN(position) AS min_pos FROM notes")
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?
        .get("min_pos");
    let position = min_pos.unwrap_or(0) - 1;

    sqlx::query(
        "INSERT INTO notes (id, title, content, project_id, position, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&title)
    .bind(&content)
    .bind(&project_id)
    .bind(position)
    .bind(&now)
    .bind(&now)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;
    // The run it was captured from becomes a `captured` session link.
    let linked =
        item_links::link_captured(db.inner(), ItemKind::Note, &id, run_id.as_deref()).await;
    // A project-less capture took the run's project with the link.
    let project_id = match project_id {
        None if linked => item_links::project_of(db.inner(), ItemKind::Note, &id).await,
        other => other,
    };

    Ok(Note {
        id,
        title,
        content,
        project_id,
        position,
        created_at: now.clone(),
        updated_at: now,
        linked_run_count: linked as i64,
    })
}

#[tauri::command]
pub async fn update_note(
    db: State<'_, Db>,
    id: String,
    title: String,
    content: String,
) -> Result<(), String> {
    let title = title.trim().to_string();
    let content = content.trim().to_string();
    if title.is_empty() && content.is_empty() {
        return Err("a title or content is required".into());
    }
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query("UPDATE notes SET title = ?, content = ?, updated_at = ? WHERE id = ?")
        .bind(&title)
        .bind(&content)
        .bind(&now)
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Link or unlink a note from a project. Pass `None` to clear the link. Session
/// links are independent of it (a session may live in another project).
#[tauri::command]
pub async fn set_note_project(
    db: State<'_, Db>,
    id: String,
    project_id: Option<String>,
) -> Result<(), String> {
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query("UPDATE notes SET project_id = ?, updated_at = ? WHERE id = ?")
        .bind(&project_id)
        .bind(&now)
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn delete_note(db: State<'_, Db>, id: String) -> Result<(), String> {
    item_links::delete_for_items(db.inner(), ItemKind::Note, &id)
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query("DELETE FROM notes WHERE id = ?")
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Bulk-delete notes by id in a single transaction, so a failure mid-way rolls
/// back the whole batch instead of leaving a partial deletion.
#[tauri::command]
pub async fn delete_notes(db: State<'_, Db>, ids: Vec<String>) -> Result<(), String> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut tx = db.inner().begin().await.map_err(|e| e.to_string())?;
    for id in &ids {
        item_links::delete_for_items(&mut *tx, ItemKind::Note, id)
            .await
            .map_err(|e| e.to_string())?;
        sqlx::query("DELETE FROM notes WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
    }
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(())
}

/// Persist a new order. The frontend sends the full list of ids top-to-bottom;
/// each row's position becomes its index, so the ordering is stable regardless
/// of prior position values. Done in one transaction to avoid partial reorders.
#[tauri::command]
pub async fn reorder_notes(db: State<'_, Db>, ids: Vec<String>) -> Result<(), String> {
    let mut tx = db.inner().begin().await.map_err(|e| e.to_string())?;
    for (index, id) in ids.iter().enumerate() {
        sqlx::query("UPDATE notes SET position = ? WHERE id = ?")
            .bind(index as i64)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
    }
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(())
}
