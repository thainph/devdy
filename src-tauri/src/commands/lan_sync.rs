//! Settings → Backup & Sync → "Sync over LAN": share this machine's configuration, or pull
//! another machine's. The logic lives in `crate::lan_sync`.

use crate::commands::settings_backup::ImportReport;
use crate::db::Db;
use crate::lan_sync::{FetchResult, LanDevice, LanSyncState, ShareStatus};
use std::time::Duration;
use tauri::{AppHandle, State};

#[tauri::command]
pub async fn lan_sync_start_share(
    app: AppHandle,
    state: State<'_, LanSyncState>,
    include_secrets: bool,
) -> Result<ShareStatus, String> {
    state.start_share(&app, include_secrets).await
}

#[tauri::command]
pub async fn lan_sync_stop_share(
    app: AppHandle,
    state: State<'_, LanSyncState>,
) -> Result<(), String> {
    state.stop_share(&app).await;
    Ok(())
}

#[tauri::command]
pub async fn lan_sync_share_status(
    state: State<'_, LanSyncState>,
) -> Result<Option<ShareStatus>, String> {
    Ok(state.share_status().await)
}

#[tauri::command]
pub async fn lan_sync_discover(
    state: State<'_, LanSyncState>,
    wait_ms: Option<u64>,
) -> Result<Vec<LanDevice>, String> {
    let wait = Duration::from_millis(wait_ms.unwrap_or(2500).clamp(500, 10_000));
    state.discover(wait).await
}

#[tauri::command]
pub async fn lan_sync_fetch(
    state: State<'_, LanSyncState>,
    endpoints: Vec<String>,
    code: String,
) -> Result<FetchResult, String> {
    state.fetch(&endpoints, &code).await
}

#[tauri::command]
pub async fn lan_sync_import(
    app: AppHandle,
    db: State<'_, Db>,
    state: State<'_, LanSyncState>,
    create_missing_dirs: Option<bool>,
) -> Result<ImportReport, String> {
    state
        .import(&app, db.inner(), create_missing_dirs.unwrap_or(false))
        .await
}

#[tauri::command]
pub async fn lan_sync_discard(state: State<'_, LanSyncState>) -> Result<(), String> {
    state.discard();
    Ok(())
}
