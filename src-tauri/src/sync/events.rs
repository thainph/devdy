//! Frontend events emitted by the sync subsystem.

use tauri::{AppHandle, Emitter};

/// A run's Devdy log (or its CLI transcript) grew because synced bytes were
/// written. Payload: `{ run_id }`. The run view re-reads the log of the run it
/// shows, so a session running on another device updates live.
pub const RUN_LOG_UPDATED: &str = "sync://run-log-updated";

/// Tell the UI that synced appends landed in `run_id`'s log. Called by the
/// files worker after it writes a `run_log` / transcript chunk.
#[allow(dead_code)]
pub fn run_log_updated(app: &AppHandle, run_id: &str) {
    let _ = app.emit(RUN_LOG_UPDATED, serde_json::json!({ "run_id": run_id }));
}
