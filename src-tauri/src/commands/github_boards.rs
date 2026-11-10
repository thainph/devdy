//! GitHub Project V2 boards, configured once under a GitHub account (URL + field
//! mapping) and enabled per project (`projects.github_board_id`, one board per
//! project). Board queries run with the owning account's PAT. No secrets here.

use crate::commands::projects::emit_projects_changed;
use crate::db::Db;
use serde::Serialize;
use sqlx::Row;
use tauri::{AppHandle, State};
use uuid::Uuid;

#[derive(Debug, Serialize, Clone)]
pub struct GithubBoard {
    pub id: String,
    pub account_id: String,
    pub url: String,
    pub title: String,
    /// JSON { boardId, ownerType, owner, number, startFieldId, deadlineFieldId, statusFieldId }.
    pub field_mappings: String,
    pub created_at: String,
}

const BOARD_COLUMNS: &str = "id, account_id, url, title, field_mappings, created_at";

fn row_to_board(row: &sqlx::sqlite::SqliteRow) -> GithubBoard {
    GithubBoard {
        id: row.get("id"),
        account_id: row.get("account_id"),
        url: row.get("url"),
        title: row.get("title"),
        field_mappings: row.get("field_mappings"),
        created_at: row.get("created_at"),
    }
}

/// The board enabled on a project, if any.
pub(crate) async fn project_board(db: &Db, project_id: &str) -> Result<Option<GithubBoard>, String> {
    let row = sqlx::query(
        "SELECT b.id, b.account_id, b.url, b.title, b.field_mappings, b.created_at \
         FROM projects p JOIN github_boards b ON b.id = p.github_board_id WHERE p.id = ?",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(row.as_ref().map(row_to_board))
}

/// Insert or update the board `url` of `account_id`; returns its id and whether
/// anything changed. Used by the settings UI and by backup / device-sync import.
pub(crate) async fn upsert_board(
    db: &Db,
    account_id: &str,
    url: &str,
    title: &str,
    field_mappings: &str,
) -> Result<(String, bool), String> {
    let err = |e: sqlx::Error| e.to_string();
    let existing: Option<String> =
        sqlx::query_scalar("SELECT id FROM github_boards WHERE account_id = ? AND url = ?")
            .bind(account_id)
            .bind(url)
            .fetch_optional(db)
            .await
            .map_err(err)?;
    if let Some(id) = existing {
        let changed = sqlx::query(
            "UPDATE github_boards SET title = ?, field_mappings = ? \
             WHERE id = ? AND (title IS NOT ? OR field_mappings IS NOT ?)",
        )
        .bind(title)
        .bind(field_mappings)
        .bind(&id)
        .bind(title)
        .bind(field_mappings)
        .execute(db)
        .await
        .map_err(err)?
        .rows_affected()
            > 0;
        return Ok((id, changed));
    }
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO github_boards (id, account_id, url, title, field_mappings, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(account_id)
    .bind(url)
    .bind(title)
    .bind(field_mappings)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(db)
    .await
    .map_err(err)?;
    Ok((id, true))
}

async fn get_board(db: &Db, id: &str) -> Result<GithubBoard, String> {
    let row = sqlx::query(&format!("SELECT {BOARD_COLUMNS} FROM github_boards WHERE id = ?"))
        .bind(id)
        .fetch_one(db)
        .await
        .map_err(|e| e.to_string())?;
    Ok(row_to_board(&row))
}

#[tauri::command]
pub async fn list_github_boards(db: State<'_, Db>) -> Result<Vec<GithubBoard>, String> {
    let rows = sqlx::query(&format!(
        "SELECT {BOARD_COLUMNS} FROM github_boards ORDER BY title COLLATE NOCASE, url"
    ))
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows.iter().map(row_to_board).collect())
}

/// Save a board under an account (a board URL already there is updated).
#[tauri::command]
pub async fn save_github_board(
    app: AppHandle,
    db: State<'_, Db>,
    account_id: String,
    url: String,
    title: String,
    field_mappings: String,
) -> Result<GithubBoard, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("Board URL is required.".to_string());
    }
    let (id, _) = upsert_board(db.inner(), &account_id, url, title.trim(), &field_mappings).await?;
    // Projects read the board's fields through the join; refresh their views.
    emit_projects_changed(&app);
    get_board(db.inner(), &id).await
}

/// Remove a board; projects that had it enabled lose it (ON DELETE SET NULL).
#[tauri::command]
pub async fn delete_github_board(app: AppHandle, db: State<'_, Db>, id: String) -> Result<(), String> {
    sqlx::query("DELETE FROM github_boards WHERE id = ?")
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    emit_projects_changed(&app);
    Ok(())
}

/// Enable `board_id` on the project (replacing any other), or disable with `None`.
#[tauri::command]
pub async fn set_project_github_board(
    app: AppHandle,
    db: State<'_, Db>,
    project_id: String,
    board_id: Option<String>,
) -> Result<(), String> {
    sqlx::query("UPDATE projects SET github_board_id = ? WHERE id = ?")
        .bind(&board_id)
        .bind(&project_id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    emit_projects_changed(&app);
    Ok(())
}
