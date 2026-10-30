//! Selective sync commands (on-demand-sync.md §3): the default source device,
//! the disabled data categories and the per-project choices of this machine.

use crate::db::Db;
use crate::sync::codec::PathCtx;
use crate::sync::prefs::{self, ProjectPref, ProjectPrefPatch, SyncPrefs};
use tauri::{AppHandle, Manager, State};

fn paths(app: &AppHandle) -> Result<PathCtx, String> {
    let data = app.path().app_data_dir().map_err(|e| format!("no app data dir: {e}"))?;
    Ok(PathCtx::from_env().with_app_data(data))
}

#[tauri::command]
pub async fn sync_prefs_get(app: AppHandle, db: State<'_, Db>) -> Result<SyncPrefs, String> {
    prefs::prefs(db.inner(), &paths(&app)?).await
}

/// Default source of the Sync button (empty clears it).
#[tauri::command]
pub async fn sync_prefs_set_source(db: State<'_, Db>, device_id: String) -> Result<(), String> {
    prefs::set_source_device(db.inner(), &device_id).await
}

/// Replace the disabled categories (`secrets` = the secrets sync toggle).
#[tauri::command]
pub async fn sync_prefs_set_categories(db: State<'_, Db>, off: Vec<String>) -> Result<(), String> {
    prefs::set_categories_off(db.inner(), &off).await
}

#[tauri::command]
pub async fn sync_project_pref_set(
    app: AppHandle,
    db: State<'_, Db>,
    project_id: String,
    patch: ProjectPrefPatch,
) -> Result<ProjectPref, String> {
    prefs::set_project_pref(db.inner(), &paths(&app)?, &project_id, patch).await
}
