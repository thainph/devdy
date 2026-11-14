//! Tauri commands for per-task worktrees (logic in `crate::worktrees`).

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::db::Db;
use crate::worktrees::{self, Worktree};

#[tauri::command]
pub async fn worktrees_list(db: State<'_, Db>, project_id: String) -> Result<Vec<Worktree>, String> {
    worktrees::list(db.inner(), &project_id).await
}

/// Delete a worktree's checkout (uncommitted work in it is lost), optionally its branch too.
#[tauri::command]
pub async fn worktree_remove(db: State<'_, Db>, id: String, delete_branch: bool) -> Result<(), String> {
    worktrees::remove(db.inner(), &id, delete_branch).await
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeSettings {
    /// Branch new worktrees start from; `None` = the remote's default branch.
    pub base_branch: Option<String>,
    /// Shell command run in a new worktree before its session starts.
    pub setup_cmd: Option<String>,
}

#[tauri::command]
pub async fn get_project_worktree_settings(
    db: State<'_, Db>,
    project_id: String,
) -> Result<WorktreeSettings, String> {
    use sqlx::Row;
    let row = sqlx::query("SELECT worktree_base_branch, worktree_setup_cmd FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(WorktreeSettings {
        base_branch: row.get("worktree_base_branch"),
        setup_cmd: row.get("worktree_setup_cmd"),
    })
}

#[tauri::command]
pub async fn set_project_worktree_settings(
    db: State<'_, Db>,
    project_id: String,
    settings: WorktreeSettings,
) -> Result<(), String> {
    let clean = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    sqlx::query("UPDATE projects SET worktree_base_branch = ?, worktree_setup_cmd = ? WHERE id = ?")
        .bind(clean(settings.base_branch))
        .bind(clean(settings.setup_cmd))
        .bind(&project_id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}
