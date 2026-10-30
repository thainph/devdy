//! Project folder sync commands (design: on-demand-sync.md §4).
//!
//! `sync_folder_preview` describes what a sync from a source device would do
//! (no writes); `sync_folder_apply` runs it in the background, one project at
//! a time, ending every project with a `sync://folder` event whose phase is
//! `done`, `error` or `skipped`; `sync_folder_cancel` stops it.
//!
//! The functions marked INTEGRATION are the seams to the engine / prefs
//! workers' code.

use crate::db::Db;
use crate::runs::RunRegistry;
use crate::sync::folder::apply::{self, ApplyReport};
use crate::sync::folder::handler::{FolderSource, RootResolver};
use crate::sync::folder::plan::FolderPreview;
use crate::sync::folder::{Cancel, FolderOptions, FolderProgress, ProgressSink, DEFAULT_MAX_FILE_MB, EVENT};
use crate::sync::peer::{PeerHandler, PeerLink};
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};

/// The running apply, if any.
static ACTIVE: Mutex<Option<Cancel>> = Mutex::new(None);

/// INTEGRATION (engine): the live peer link of the running sync engine.
/// TODO(integrator): return the engine's `PeerLink` (e.g. from `SyncManager`).
async fn peer_link(_app: &AppHandle) -> Result<Arc<dyn PeerLink>, String> {
    Err("sync is not connected".into())
}

/// INTEGRATION (engine): the source-side handler; register it with
/// `SyncManager::register_peer_handler(folder_handler(db))` before start.
#[allow(dead_code)] // TODO(integrator): remove once registered.
pub fn folder_handler(db: Db) -> Arc<dyn PeerHandler> {
    Arc::new(FolderSource::new(Arc::new(DbRoots(db))))
}

#[allow(dead_code)] // TODO(integrator): see `folder_handler`.
struct DbRoots(Db);

#[async_trait]
impl RootResolver for DbRoots {
    /// INTEGRATION (prefs): the project's folder on this (source) machine.
    /// TODO(integrator): use `prefs::project_pref(db, id).local_path` when set.
    async fn root(&self, project_id: &str) -> Result<PathBuf, String> {
        let path: Option<String> = sqlx::query_scalar("SELECT path FROM projects WHERE id = ?")
            .bind(project_id)
            .fetch_optional(&self.0)
            .await
            .map_err(|e| e.to_string())?;
        match path.filter(|p| !p.trim().is_empty()) {
            Some(p) => Ok(PathBuf::from(p)),
            None => Err("this project has no folder on the source device".into()),
        }
    }
}

/// INTEGRATION (prefs): name + folder sync options of a project on this
/// (receiving) machine; `None` when it has no local path.
/// TODO(integrator): build from `prefs::project_pref(db, id)` (local_path,
/// excludes, default_excludes, max_file_mb) instead of `projects.path`.
async fn folder_options(db: &Db, project_id: &str) -> Result<(String, Option<FolderOptions>), String> {
    let row: Option<(String, String)> = sqlx::query_as("SELECT name, path FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_optional(db)
        .await
        .map_err(|e| e.to_string())?;
    let Some((name, path)) = row else { return Ok((String::new(), None)) };
    let opts = (!path.trim().is_empty()).then(|| FolderOptions {
        project_id: project_id.to_string(),
        local_root: PathBuf::from(path),
        excludes: vec![],
        default_excludes: true,
        max_file_mb: DEFAULT_MAX_FILE_MB,
    });
    Ok((name, opts))
}

/// INTEGRATION (prefs): a session of this project runs on this machine.
/// TODO(integrator): may be replaced by `ProjectPref::running_here`.
async fn running_here(app: &AppHandle, db: &Db, project_id: &str) -> bool {
    let ids: Vec<String> = match app.try_state::<RunRegistry>() {
        Some(r) => r.lock().await.keys().cloned().collect(),
        None => return false,
    };
    for id in ids {
        let hit: Option<i64> = sqlx::query_scalar("SELECT 1 FROM runs WHERE id = ? AND project_id = ?")
            .bind(&id)
            .bind(project_id)
            .fetch_optional(db)
            .await
            .unwrap_or(None);
        if hit.is_some() {
            return true;
        }
    }
    false
}

async fn online_link(app: &AppHandle, source_device: &str) -> Result<Arc<dyn PeerLink>, String> {
    let link = peer_link(app).await?;
    if !link.online_peers().await?.iter().any(|d| d == source_device) {
        return Err("peer_offline: the source device is not online".into());
    }
    Ok(link)
}

fn skipped_preview(project_id: &str, name: String, reason: &str) -> FolderPreview {
    FolderPreview { project_id: project_id.into(), name, skipped: Some(reason.into()), ..Default::default() }
}

#[tauri::command]
pub async fn sync_folder_preview(
    app: AppHandle,
    db: State<'_, Db>,
    source_device: String,
    project_ids: Vec<String>,
) -> Result<Vec<FolderPreview>, String> {
    let link = online_link(&app, &source_device).await?;
    let mut out = Vec::new();
    for pid in project_ids {
        let (name, opts) = folder_options(&db, &pid).await?;
        let Some(opts) = opts else {
            out.push(skipped_preview(&pid, name, "no_local_path"));
            continue;
        };
        if running_here(&app, &db, &pid).await {
            out.push(skipped_preview(&pid, name, "running_session"));
            continue;
        }
        match apply::preview(link.clone(), &source_device, &opts).await {
            Ok(mut p) => {
                p.name = name;
                out.push(p);
            }
            Err(e) if e.contains("peer_offline") => return Err(e),
            Err(e) => out.push(FolderPreview {
                project_id: pid,
                name,
                local_path: opts.local_root.to_string_lossy().into(),
                error: Some(e),
                ..Default::default()
            }),
        }
    }
    Ok(out)
}

#[tauri::command]
pub async fn sync_folder_apply(
    app: AppHandle,
    db: State<'_, Db>,
    source_device: String,
    project_ids: Vec<String>,
) -> Result<(), String> {
    let link = online_link(&app, &source_device).await?;
    let cancel = {
        let mut active = ACTIVE.lock().unwrap();
        if active.is_some() {
            return Err("a folder sync is already running".into());
        }
        let c = Cancel::default();
        *active = Some(c.clone());
        c
    };
    let db = db.inner().clone();
    tauri::async_runtime::spawn(async move {
        let emitter = app.clone();
        let sink: ProgressSink = Arc::new(move |p: &FolderProgress| {
            let _ = emitter.emit(EVENT, p);
        });
        let finish = |pid: &str, phase: &str, message: Option<String>| {
            sink(&FolderProgress { project_id: pid.into(), phase: phase.into(), message, ..Default::default() });
        };
        for pid in &project_ids {
            if cancel.is_cancelled() {
                finish(pid, "error", Some("cancelled".into()));
                continue;
            }
            let opts = match folder_options(&db, pid).await {
                Ok((_, Some(o))) => o,
                Ok((_, None)) => {
                    finish(pid, "skipped", Some("no_local_path".into()));
                    continue;
                }
                Err(e) => {
                    finish(pid, "error", Some(e));
                    continue;
                }
            };
            if running_here(&app, &db, pid).await {
                finish(pid, "skipped", Some("running_session".into()));
                continue;
            }
            match apply::apply(link.clone(), &source_device, &opts, sink.clone(), cancel.clone()).await {
                Ok(ApplyReport { backup, .. }) => {
                    // TODO(integrator): record `prefs` last_backup_at = backup.ts.
                    finish(pid, "done", Some(format!("backup: {}", backup.describe())));
                }
                Err(e) => {
                    tracing::warn!(event = "folder_sync_failed", project_id = %pid, error = %e);
                    finish(pid, "error", Some(e));
                }
            }
        }
        *ACTIVE.lock().unwrap() = None;
    });
    Ok(())
}

#[tauri::command]
pub fn sync_folder_cancel() -> Result<(), String> {
    if let Some(c) = ACTIVE.lock().unwrap().as_ref() {
        c.cancel();
    }
    Ok(())
}
