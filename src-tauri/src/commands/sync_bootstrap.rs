//! Bootstrap commands for two-way sync (design §5): "re-sync everything", the
//! first-join merge preview/commit, and the project path mapping.
//!
//! After `sync_join` the engine stays off (`bootstrap::PENDING_KEY`) until
//! `sync_bootstrap_commit` has merged the other device's state into this one.
//! The merge reads it straight from that device (`changes{since: 0}`, on-demand
//! sync §P2): it is online, it just granted the join.

use crate::commands::sync::SyncManager;
use crate::db::Db;
use crate::runs::RunRegistry;
use crate::sync::bootstrap::{self, BootstrapPreview, BootstrapReport, ProjectMapping, RemoteState};
use crate::sync::codec::PathCtx;
use crate::sync::engine::{PeerHandlers, StatusSink, SyncEngine, Timing};
use crate::sync::feed::{FileScope, PeerFiles};
use crate::sync::store::{SqliteSyncStore, SyncStore};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Manager, State};

/// Home + app data: `@data/...` paths (library, Claude config dirs, managed
/// SSH keys) resolve under this machine's app data.
fn paths(app: &AppHandle) -> Result<PathCtx, String> {
    let data = app.path().app_data_dir().map_err(|e| format!("no app data dir: {e}"))?;
    Ok(PathCtx::from_env().with_app_data(data))
}

/// How long the merge waits for the relay connection.
const CONNECT_WAIT: Duration = Duration::from_secs(20);

fn store_of(db: &Db, device_id: &str, paths: PathCtx) -> SqliteSyncStore {
    SqliteSyncStore::new(db.clone(), device_id.to_string())
        .with_paths(paths)
        .with_secrets(Arc::new(crate::sync::secrets::KeychainBackend))
}

/// A paused engine (it serves nothing: this device's ids are not merged yet)
/// to read the other device's feed with, and that device's id.
async fn open_group(db: &Db, paths: PathCtx) -> Result<(SyncEngine, String), String> {
    let sync = crate::secrets::get_sync_secrets().ok_or("this device is not in a sync group")?;
    let params = crate::sync::pair::connect_params(db, &sync).await?;
    let keys = crate::sync::pair::keys_of(&sync)?;
    let store: Arc<dyn SyncStore> = Arc::new(store_of(db, &sync.device_id, paths));
    let quiet: StatusSink = Arc::new(|_| {});
    let engine = SyncEngine::start_with(params, keys, store, quiet, true, Timing::default(), PeerHandlers::default());
    let h = engine.handle();
    if !h.wait_connected(CONNECT_WAIT).await {
        let e = h.status().last_error.unwrap_or_else(|| "relay unreachable".into());
        engine.stop().await;
        return Err(e);
    }
    let peers = h.online_peers().await;
    match peers.map(|p| p.into_iter().next()) {
        Ok(Some(source)) => Ok((engine, source)),
        Ok(None) => {
            engine.stop().await;
            Err("peer_offline: the device that added this one must be online (Devdy running) to merge".into())
        }
        Err(e) => {
            engine.stop().await;
            Err(e)
        }
    }
}

/// The other device's whole state, read into memory.
async fn group_state(db: &Db, paths: PathCtx) -> Result<RemoteState, String> {
    let (engine, source) = open_group(db, paths).await?;
    let state = bootstrap::fetch_group_state(engine.handle().link().as_ref(), &source).await;
    engine.stop().await;
    state
}

/// "Re-sync everything": every synced row gets a fresh version in this
/// device's change feed (the other device takes them with its next Sync).
/// Returns the count.
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
    let paths = paths(&app)?;
    let state = group_state(db, paths.clone()).await?;
    let git = bootstrap::local_git_remotes(db).await?;
    bootstrap::preview(db, &paths, &state, &git).await
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

/// Merge the other device's state into this one (re-key, LWW apply, file its
/// files and Keychain items, queue the local-only rows), fetch the library
/// files, keys and Keychain items from it (session files come with the next
/// Sync), then start syncing. Refused while a run is live, and when no merge is pending. Run
/// starts are blocked for the whole commit (`bootstrap::refuse_if_committing`).
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
    let (engine, source) = open_group(db, paths.clone()).await?;
    let link = engine.handle().link();
    let merged = async {
        let remote = bootstrap::fetch_group_state(link.as_ref(), &source).await?;
        let git = bootstrap::local_git_remotes(db).await?;
        // The download and git scan take a while: a run started just before
        // the guard may have registered since.
        refuse_if_live(registry.lock().await.len())?;
        let store = store_of(db, &sync.device_id, paths.clone());
        let report = bootstrap::commit(db, &store, &paths, remote, &git).await?;
        // Small and needed right away: library files, keys, Keychain items.
        // What fails stays filed for the next Sync.
        let files = PeerFiles::new(link.clone(), &source);
        if let Err(e) = store.fetch_extra(&files, FileScope::NoSessions, &|| {}).await {
            tracing::warn!(event = "sync_join_files_failed", error = %e);
        }
        Ok::<_, String>(report)
    }
    .await;
    engine.stop().await;
    let report = merged?;
    drop(guard);
    let tables: Vec<String> = report.preview.tables.iter().map(|t| t.tbl.clone()).collect();
    crate::sync::events::rows_applied(&app, &tables);
    state.start(&app, db).await?;
    Ok(report)
}

/// Merge again: stop syncing and go through the merge step, which re-reads
/// the other device's whole state (no relay store can fall behind any more;
/// kept for a merge the user asks for).
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
