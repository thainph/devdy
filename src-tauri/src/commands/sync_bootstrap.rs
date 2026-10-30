//! Bootstrap commands for two-way sync (design §5): full re-upload, the
//! first-join merge preview/commit, and the project path mapping.
//!
//! After `sync_join` the engine stays off (`bootstrap::PENDING_KEY`) until
//! `sync_bootstrap_commit` has merged the group state into this device.

use crate::commands::sync::SyncManager;
use crate::db::Db;
use crate::runs::RunRegistry;
use crate::sync::bootstrap::{self, BootstrapPreview, BootstrapReport, ProjectMapping, RemoteState};
use crate::sync::codec::PathCtx;
use crate::sync::crypto::SyncKeys;
use crate::sync::engine::RelayBlobs;
use crate::sync::store::SqliteSyncStore;
use crate::sync::transport::Connection;
use tauri::{AppHandle, Manager, State};

/// Home + app data: `@data/...` paths (library, Claude config dirs, managed
/// SSH keys) resolve under this machine's app data.
fn paths(app: &AppHandle) -> Result<PathCtx, String> {
    let data = app.path().app_data_dir().map_err(|e| format!("no app data dir: {e}"))?;
    Ok(PathCtx::from_env().with_app_data(data))
}

/// A connection to the group (blobs come over it too) and its keys.
async fn open_group(db: &Db) -> Result<(Connection, SyncKeys), String> {
    let sync = crate::secrets::get_sync_secrets().ok_or("this device is not in a sync group")?;
    let params = crate::sync::pair::connect_params(db, &sync).await?;
    let keys = crate::sync::pair::keys_of(&sync)?;
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let conn = Connection::connect(&params, false, tx).await.map_err(|e| e.to_string())?;
    Ok((conn, keys))
}

/// The whole group log, downloaded into memory.
async fn group_state(db: &Db) -> Result<RemoteState, String> {
    let (conn, keys) = open_group(db).await?;
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
pub async fn sync_bootstrap_preview(app: AppHandle, db: State<'_, Db>) -> Result<BootstrapPreview, String> {
    let db = db.inner();
    let state = group_state(db).await?;
    let git = bootstrap::local_git_remotes(db).await?;
    bootstrap::preview(db, &paths(&app)?, &state, &git).await
}

/// Error code the UI shows as "stop the running sessions first".
pub const RUNS_LIVE: &str = "sync_bootstrap_runs_live";
/// Error code: there is no merge waiting (already committed, or not joined).
pub const NOT_PENDING: &str = "sync_bootstrap_not_pending";

/// Re-keying moves project / run ids that a live sidecar still uses
/// (`DEVDY_PROJECT_ID`, its run row): only allowed with no run in flight.
fn refuse_if_live(live: usize) -> Result<(), String> {
    if live > 0 {
        return Err(format!("{RUNS_LIVE}: {live} session(s) still running on this device; stop them before merging"));
    }
    Ok(())
}

/// Merge the group state into this device (re-key, LWW apply, files and
/// Keychain items, queue the local-only rows), then start syncing. Refused
/// while a run is live, and when no merge is pending. Run starts are blocked
/// for the whole commit (`bootstrap::refuse_if_committing`).
#[tauri::command]
pub async fn sync_bootstrap_commit(
    app: AppHandle,
    db: State<'_, Db>,
    state: State<'_, SyncManager>,
    registry: State<'_, RunRegistry>,
) -> Result<BootstrapReport, String> {
    let db = db.inner();
    let sync = crate::secrets::get_sync_secrets().ok_or("this device is not in a sync group")?;
    if !bootstrap::is_pending(db).await? {
        return Err(format!("{NOT_PENDING}: this device has no merge waiting"));
    }
    let guard = bootstrap::begin_commit()?;
    refuse_if_live(registry.lock().await.len())?;
    state.stop().await;
    let paths = paths(&app)?;
    let (conn, keys) = open_group(db).await?;
    let remote = bootstrap::fetch_group_state(&conn, &keys).await?;
    let git = bootstrap::local_git_remotes(db).await?;
    // The download and git scan take a while: a run started just before the
    // guard may have registered since.
    refuse_if_live(registry.lock().await.len())?;
    let store = SqliteSyncStore::new(db.clone(), sync.device_id.clone())
        .with_paths(paths.clone())
        .with_secrets(std::sync::Arc::new(crate::sync::secrets::KeychainBackend));
    let blobs = RelayBlobs::new(&conn, &keys);
    let report = bootstrap::commit(db, &store, &paths, remote, &git, &blobs).await?;
    drop(conn);
    drop(guard);
    let tables: Vec<String> = report.preview.tables.iter().map(|t| t.tbl.clone()).collect();
    crate::sync::events::rows_applied(&app, &tables);
    state.start(&app, db).await?;
    Ok(report)
}

/// The relay lost ops this device already pulled (restored from a backup,
/// `relay_behind`): stop syncing and go through the merge step again, which
/// re-reads the whole group and pushes what the relay no longer has.
#[tauri::command]
pub async fn sync_rebootstrap(app: AppHandle, db: State<'_, Db>, state: State<'_, SyncManager>) -> Result<(), String> {
    if crate::secrets::get_sync_secrets().is_none() {
        return Err("this device is not in a sync group".into());
    }
    state.stop().await;
    bootstrap::mark_joined(db.inner()).await?;
    use tauri::Emitter;
    let _ = app.emit(crate::sync::engine::STATUS_EVENT, crate::sync::engine::SyncStatus::disabled());
    Ok(())
}

#[tauri::command]
pub async fn sync_project_mapping_list(app: AppHandle, db: State<'_, Db>) -> Result<Vec<ProjectMapping>, String> {
    bootstrap::project_mappings(db.inner(), &paths(&app)?).await
}

/// Point a synced project at its checkout on this machine.
#[tauri::command]
pub async fn sync_project_mapping_set(
    app: AppHandle,
    db: State<'_, Db>,
    project_id: String,
    local_path: String,
) -> Result<ProjectMapping, String> {
    bootstrap::set_project_mapping(db.inner(), &paths(&app)?, &project_id, &local_path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_is_refused_while_a_run_is_live() {
        assert!(refuse_if_live(0).is_ok());
        let e = refuse_if_live(2).unwrap_err();
        assert!(e.starts_with(RUNS_LIVE), "{e}");
    }
}
