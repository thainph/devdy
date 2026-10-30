//! Bootstrap commands for two-way sync (design §5): full re-upload, the
//! first-join merge preview/commit, and the project path mapping.
//!
//! After `sync_join` the engine stays off (`bootstrap::PENDING_KEY`) until
//! `sync_bootstrap_commit` has merged the group state into this device.

use crate::commands::sync::SyncManager;
use crate::db::Db;
use crate::sync::bootstrap::{self, BootstrapPreview, BootstrapReport, ProjectMapping, RemoteState};
use crate::sync::codec::PathCtx;
use crate::sync::store::SqliteSyncStore;
use crate::sync::transport::Connection;
use tauri::{AppHandle, State};

/// The whole group log, downloaded into memory.
async fn group_state(db: &Db) -> Result<RemoteState, String> {
    let sync = crate::secrets::get_sync_secrets().ok_or("this device is not in a sync group")?;
    let params = crate::sync::pair::connect_params(db, &sync).await?;
    let keys = crate::sync::pair::keys_of(&sync)?;
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let conn = Connection::connect(&params, false, tx).await.map_err(|e| e.to_string())?;
    bootstrap::fetch_group_state(&conn, &keys).await
}

/// "Re-sync everything": queue every synced row for push. Returns the count.
#[tauri::command]
pub async fn sync_resync_all(db: State<'_, Db>, state: State<'_, SyncManager>) -> Result<usize, String> {
    let n = bootstrap::enqueue_all(db.inner()).await?;
    state.kick().await;
    Ok(n)
}

/// Whether this device joined a group and still has to merge its data (a
/// stale flag left by leaving mid-merge does not count).
#[tauri::command]
pub async fn sync_bootstrap_pending(db: State<'_, Db>) -> Result<bool, String> {
    if crate::secrets::get_sync_secrets().is_none() {
        return Ok(false);
    }
    bootstrap::is_pending(db.inner()).await
}

/// What merging the group state into this device would do (nothing written).
#[tauri::command]
pub async fn sync_bootstrap_preview(db: State<'_, Db>) -> Result<BootstrapPreview, String> {
    let db = db.inner();
    let state = group_state(db).await?;
    let git = bootstrap::local_git_remotes(db).await?;
    bootstrap::preview(db, &PathCtx::from_env(), &state, &git).await
}

/// Merge the group state into this device (re-key, LWW apply, queue the
/// local-only rows), then start syncing.
#[tauri::command]
pub async fn sync_bootstrap_commit(
    app: AppHandle,
    db: State<'_, Db>,
    state: State<'_, SyncManager>,
) -> Result<BootstrapReport, String> {
    let db = db.inner();
    state.stop().await;
    let sync = crate::secrets::get_sync_secrets().ok_or("this device is not in a sync group")?;
    let remote = group_state(db).await?;
    let git = bootstrap::local_git_remotes(db).await?;
    let store = SqliteSyncStore::new(db.clone(), sync.device_id.clone());
    let report = bootstrap::commit(db, &store, &PathCtx::from_env(), remote, &git).await?;
    let tables: Vec<String> = report.preview.tables.iter().map(|t| t.tbl.clone()).collect();
    crate::sync::events::rows_applied(&app, &tables);
    state.start(&app, db).await?;
    Ok(report)
}

#[tauri::command]
pub async fn sync_project_mapping_list(db: State<'_, Db>) -> Result<Vec<ProjectMapping>, String> {
    bootstrap::project_mappings(db.inner(), &PathCtx::from_env()).await
}

/// Point a synced project at its checkout on this machine.
#[tauri::command]
pub async fn sync_project_mapping_set(
    db: State<'_, Db>,
    project_id: String,
    local_path: String,
) -> Result<ProjectMapping, String> {
    bootstrap::set_project_mapping(db.inner(), &PathCtx::from_env(), &project_id, &local_path).await
}
