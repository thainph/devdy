//! Frontend events emitted by the sync subsystem.
//!
//! The store reports what it applied through a [`Notifier`] (no Tauri types in
//! the core, so tests run without an app); [`app_notifier`] turns those into
//! Tauri events.

use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};

/// A run's Devdy log (or its CLI transcript) grew because synced bytes were
/// written. Payload: `{ run_id }`. The run view re-reads the log of the run it
/// shows, so a session running on another device updates live.
pub const RUN_LOG_UPDATED: &str = "sync://run-log-updated";

/// Synced DB rows were applied. Payload: `{ tables: [..] }` (table names).
/// Lists (runs, owners) refetch when a table they show is in it.
pub const ROWS_APPLIED: &str = "sync://rows-applied";

/// Another device took over a run whose sidecar was live here; the local
/// sidecar was stopped. Payload: `{ run_id, device_name }`.
pub const RUN_HANDED_OFF: &str = "sync://run-handed-off";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    RowsApplied(Vec<String>),
    /// Ids of the synced `runs` rows in an applied batch (alongside
    /// `RowsApplied`): feeds the local lease receipts and the hand-off check.
    RunsApplied(Vec<String>),
    RunLogUpdated(String),
}

pub type Notifier = Arc<dyn Fn(Notice) + Send + Sync>;

/// Tell the UI that synced appends landed in `run_id`'s log.
pub fn run_log_updated(app: &AppHandle, run_id: &str) {
    let _ = app.emit(RUN_LOG_UPDATED, serde_json::json!({ "run_id": run_id }));
}

pub fn rows_applied(app: &AppHandle, tables: &[String]) {
    if !tables.is_empty() {
        let _ = app.emit(ROWS_APPLIED, serde_json::json!({ "tables": tables }));
    }
}

pub fn app_notifier(app: AppHandle) -> Notifier {
    Arc::new(move |n| match n {
        Notice::RowsApplied(tables) => rows_applied(&app, &tables),
        Notice::RunLogUpdated(run_id) => run_log_updated(&app, &run_id),
        Notice::RunsApplied(run_ids) => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let (Some(db), Some(registry)) =
                    (app.try_state::<crate::db::Db>(), app.try_state::<crate::runs::RunRegistry>())
                else {
                    return;
                };
                for h in super::sessions::runs_applied(db.inner(), registry.inner(), &run_ids).await {
                    let _ = app.emit(
                        RUN_HANDED_OFF,
                        serde_json::json!({ "run_id": h.run_id, "device_name": h.device_name }),
                    );
                }
            });
        }
    })
}
