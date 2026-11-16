//! Delivery Radar data shared with the built-in `devdy` MCP server.
//!
//! The radar window keeps its boards and settings in its own localStorage; the
//! MCP server (`sidecar-mcp/lib/radar-store.mjs`) only sees the SQLite store and
//! holds no GitHub credentials. So every fetched board is also saved as the
//! project's snapshot (`radar_snapshots`), the radar settings are mirrored to
//! `settings.radar_settings`, and an MCP refresh asks the broker, which fetches
//! here with the project's accounts (`runs::broker::radar`).

use futures_util::StreamExt;

use crate::commands::github::{load_milestone_board, MilestoneBoard};
use crate::db::Db;

/// `settings` key holding the radar settings JSON
/// (`{ thresholds: {dueSoonDays, staleDays}, stages: {status: stage}, testers: [login] }`).
pub const SETTINGS_KEY: &str = "radar_settings";

/// Same cap as the radar window: projects fetched from GitHub at once.
const CONCURRENCY: usize = 3;

/// Fetch a project's board and keep it as its radar snapshot. A failed save is
/// logged only: the caller still gets the board.
pub async fn fetch_and_save(db: &Db, project_id: &str) -> Result<MilestoneBoard, String> {
    let board = load_milestone_board(db, project_id).await?;
    if let Err(e) = save_snapshot(db, project_id, &board).await {
        tracing::warn!("radar: could not save the snapshot of {project_id}: {e}");
    }
    Ok(board)
}

async fn save_snapshot(db: &Db, project_id: &str, board: &MilestoneBoard) -> Result<(), String> {
    let json = serde_json::to_string(board).map_err(|e| e.to_string())?;
    sqlx::query(
        "INSERT INTO radar_snapshots (project_id, board, saved_at) VALUES (?, ?, ?) \
         ON CONFLICT(project_id) DO UPDATE SET board = excluded.board, saved_at = excluded.saved_at",
    )
    .bind(project_id)
    .bind(json)
    .bind(chrono::Utc::now().timestamp_millis())
    .execute(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Projects the radar covers: those with at least one GitHub repo.
pub async fn radar_project_ids(db: &Db) -> Result<Vec<String>, String> {
    sqlx::query_scalar(
        "SELECT DISTINCT project_id FROM repos \
         WHERE COALESCE(NULLIF(provider, ''), 'github') = 'github' \
           AND COALESCE(github_owner, '') <> '' AND COALESCE(github_repo, '') <> ''",
    )
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())
}

/// Refetch the given projects (CONCURRENCY at a time). Returns the error of
/// each project that failed, keyed by id; the rest have a fresh snapshot.
pub async fn refresh_projects(db: &Db, project_ids: Vec<String>) -> Vec<(String, String)> {
    futures_util::stream::iter(project_ids)
        .map(|id| async move {
            let res = fetch_and_save(db, &id).await;
            res.err().map(|e| (id, e))
        })
        .buffer_unordered(CONCURRENCY)
        .filter_map(|e| async move { e })
        .collect()
        .await
}

/// Mirror the radar window's settings so the MCP server classifies like it.
#[tauri::command]
pub async fn set_radar_settings(
    db: tauri::State<'_, Db>,
    settings: serde_json::Value,
) -> Result<(), String> {
    if !settings.is_object() {
        return Err("radar settings must be an object".into());
    }
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES (?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(SETTINGS_KEY)
    .bind(settings.to_string())
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}
