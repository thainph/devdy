//! Project folder sync commands (design: on-demand-sync.md §4).
//!
//! `sync_folder_preview` describes what a sync from a source device would do
//! (no writes); `sync_folder_apply` runs it in the background, one project at
//! a time, ending every project with a `sync://folder` event whose phase is
//! `done`, `error` or `skipped`; `sync_folder_cancel` stops it.
//!
//! Folder options come from `sync::prefs`; requests go through the sync
//! engine's `PeerLink`.

use crate::commands::sync::SyncManager;
use crate::db::Db;
use crate::sync::prefs;
use crate::sync::folder::apply::{self, ApplyReport};
use crate::sync::folder::handler::{FolderSource, RootResolver};
use crate::sync::folder::plan::{Expected, FolderPreview};
use crate::sync::folder::{Cancel, FolderOptions, FolderProgress, ProgressSink, EVENT};
use crate::sync::peer::{PeerHandler, PeerLink};
use async_trait::async_trait;
use std::path::PathBuf;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};


/// The running apply, if any.
static ACTIVE: Mutex<Option<Cancel>> = Mutex::new(None);

/// What each (source, project) preview committed to; apply refuses to
/// delete or replace more than that.
static PREVIEWED: LazyLock<Mutex<HashMap<(String, String), Expected>>> = LazyLock::new(Default::default);

/// Clears [`ACTIVE`] when the apply task ends, even by panic.
struct ActiveGuard;

impl ActiveGuard {
    fn take() -> Result<(Self, Cancel), String> {
        let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
        if active.is_some() {
            return Err("a folder sync is already running".into());
        }
        let c = Cancel::default();
        *active = Some(c.clone());
        Ok((ActiveGuard, c))
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        *ACTIVE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// The live peer link of the running sync engine.
async fn peer_link(app: &AppHandle) -> Result<Arc<dyn PeerLink>, String> {
    let manager = app.try_state::<SyncManager>().ok_or("sync_not_connected")?;
    manager.peer_link().await.ok_or_else(|| "sync_not_connected".into())
}

/// The source-side handler, registered with the `SyncManager` at startup.
pub fn folder_handler(db: Db) -> Arc<dyn PeerHandler> {
    Arc::new(FolderSource::new(Arc::new(DbRoots(db))))
}

struct DbRoots(Db);

#[async_trait]
impl RootResolver for DbRoots {
    /// The project's folder on this (source) machine.
    async fn root(&self, project_id: &str) -> Result<PathBuf, String> {
        let pref = prefs::project_pref(&self.0, project_id).await?;
        if pref.local_path.trim().is_empty() || !pref.exists {
            return Err("this project has no folder on the source device".into());
        }
        Ok(PathBuf::from(pref.local_path))
    }
}

/// Name + folder sync options of a project on this (receiving) machine
/// (`None` when it has no local path), and whether one of its sessions runs here.
async fn folder_options(db: &Db, project_id: &str) -> Result<(String, Option<FolderOptions>, bool), String> {
    let pref = prefs::project_pref(db, project_id).await?;
    let opts = (!pref.local_path.trim().is_empty()).then(|| FolderOptions {
        project_id: project_id.to_string(),
        local_root: PathBuf::from(&pref.local_path),
        excludes: pref.excludes.clone(),
        default_excludes: pref.default_excludes,
        max_file_mb: pref.max_file_mb.max(0) as u64,
    });
    Ok((pref.name, opts, pref.running_here))
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
        let (name, opts, running) = folder_options(&db, &pid).await?;
        let Some(opts) = opts else {
            out.push(skipped_preview(&pid, name, "no_local_path"));
            continue;
        };
        if running {
            out.push(skipped_preview(&pid, name, "running_session"));
            continue;
        }
        match apply::preview_expected(link.clone(), &source_device, &opts).await {
            Ok((mut p, expected)) => {
                PREVIEWED.lock().unwrap_or_else(|e| e.into_inner()).insert((source_device.clone(), pid.clone()), expected);
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
    let (active, cancel) = ActiveGuard::take()?;
    let db = db.inner().clone();
    tauri::async_runtime::spawn(async move {
        let _active = active;
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
            let (opts, running) = match folder_options(&db, pid).await {
                Ok((_, Some(o), running)) => (o, running),
                Ok((_, None, _)) => {
                    finish(pid, "skipped", Some("no_local_path".into()));
                    continue;
                }
                Err(e) => {
                    finish(pid, "error", Some(e));
                    continue;
                }
            };
            if running {
                finish(pid, "skipped", Some("running_session".into()));
                continue;
            }
            let Some(expected) = PREVIEWED.lock().unwrap_or_else(|e| e.into_inner()).remove(&(source_device.clone(), pid.clone())) else {
                finish(pid, "error", Some("preview_required: preview the folder sync first".into()));
                continue;
            };
            // A session may have started here since the preview.
            let (db2, pid2) = (db.clone(), pid.clone());
            let precheck: apply::Precheck = Arc::new(move || {
                let (db, pid) = (db2.clone(), pid2.clone());
                Box::pin(async move {
                    match prefs::project_pref(&db, &pid).await {
                        Ok(p) if p.running_here => Err("running_session: a session of this project started meanwhile".into()),
                        Ok(_) => Ok(()),
                        Err(e) => Err(e),
                    }
                })
            });
            let guard = apply::ApplyGuard { expected: Some(expected), precheck: Some(precheck) };
            match apply::apply(link.clone(), &source_device, &opts, sink.clone(), cancel.clone(), guard).await {
                Ok(ApplyReport { backup, .. }) => {
                    let now = chrono::Utc::now().to_rfc3339();
                    if let Err(e) = prefs::set_last_backup_at(&db, pid, &now).await {
                        tracing::warn!(event = "folder_sync_backup_time_failed", error = %e);
                    }
                    finish(pid, "done", Some(format!("backup: {}", backup.describe())));
                }
                Err(e) => {
                    tracing::warn!(event = "folder_sync_failed", project_id = %pid, error = %e);
                    finish(pid, "error", Some(e));
                }
            }
        }
    });
    Ok(())
}

#[tauri::command]
pub fn sync_folder_cancel() -> Result<(), String> {
    if let Some(c) = ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        c.cancel();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_slot_is_freed_even_on_panic() {
        let r = std::panic::catch_unwind(|| {
            let (_g, _c) = ActiveGuard::take().unwrap();
            assert!(ActiveGuard::take().is_err(), "one apply at a time");
            panic!("apply task died");
        });
        assert!(r.is_err());
        let (_g, _c) = ActiveGuard::take().expect("slot freed by the guard");
    }
}
