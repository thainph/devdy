//! Per-run engine/model/permission-mode selection — the single source of truth
//! shared between the desktop composer and a remote controller.
//!
//! Both surfaces write their selection through [`apply_run_meta`], which:
//!   1. merges the patch into the in-memory [`RunMetaStore`] (so a freshly
//!      pairing controller can be handed the current snapshot), then
//!   2. emits `run:meta:<run_id>` to the desktop webview (so the OTHER surface's
//!      change reflects in the composer), and
//!   3. publishes [`RemoteRunEvent::Meta`] onto the [`RemoteBus`] (so the forwarder
//!      seals it to the controller).
//!
//! The result is lock-step, bidirectional, realtime sync: whoever changes a
//! selector, both composers converge on the same values.

use crate::remote::bus::{RemoteBus, RemoteRunEvent};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, State};

/// A run's current selector state. Fields are `Option` so a partial patch (e.g.
/// only the permission mode changed) never clobbers the others with a blank.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RunMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
}

/// Tauri-managed store of the latest selection per run id.
#[derive(Default)]
pub struct RunMetaStore {
    inner: Mutex<HashMap<String, RunMeta>>,
}

impl RunMetaStore {
    /// The current snapshot for `run_id`, if any surface has published one.
    pub fn get(&self, run_id: &str) -> Option<RunMeta> {
        self.inner.lock().ok()?.get(run_id).cloned()
    }

    /// Merge a patch into the stored meta and return the merged result. `None`
    /// fields in the patch leave the existing value untouched; a `Some("")` also
    /// leaves it untouched (blank means "no opinion", not "clear").
    pub fn merge(
        &self,
        run_id: &str,
        engine: Option<String>,
        model: Option<String>,
        permission_mode: Option<String>,
    ) -> RunMeta {
        let mut guard = self.inner.lock().expect("run meta store poisoned");
        let entry = guard.entry(run_id.to_string()).or_default();
        if let Some(v) = engine.filter(|s| !s.trim().is_empty()) {
            entry.engine = Some(v);
        }
        if let Some(v) = model.filter(|s| !s.trim().is_empty()) {
            entry.model = Some(v);
        }
        if let Some(v) = permission_mode.filter(|s| !s.trim().is_empty()) {
            entry.permission_mode = Some(v);
        }
        entry.clone()
    }
}

/// Merge a selection patch and fan it out to BOTH surfaces (desktop event +
/// controller bus). Called by the desktop command below and by the remote
/// `set_run_meta` handler, so a change from either side reaches the other.
pub fn apply_run_meta(
    app: &AppHandle,
    store: &RunMetaStore,
    bus: &RemoteBus,
    run_id: &str,
    engine: Option<String>,
    model: Option<String>,
    permission_mode: Option<String>,
) -> RunMeta {
    let merged = store.merge(run_id, engine, model, permission_mode);
    // Reflect on the desktop composer (the origin ignores its own echo).
    let _ = app.emit(&format!("run:meta:{run_id}"), &merged);
    // Reflect on the controller (no-op when no forwarder is subscribed).
    bus.publish(RemoteRunEvent::Meta {
        run_id: run_id.to_string(),
        engine: merged.engine.clone(),
        model: merged.model.clone(),
        permission_mode: merged.permission_mode.clone(),
    });
    merged
}

/// Desktop-side command: the composer pushes its current engine/model/permission
/// selection here so the store stays authoritative and the controller mirrors it.
#[tauri::command]
pub fn set_run_meta(
    app: AppHandle,
    store: State<'_, RunMetaStore>,
    bus: State<'_, RemoteBus>,
    run_id: String,
    engine: Option<String>,
    model: Option<String>,
    permission_mode: Option<String>,
) {
    apply_run_meta(
        &app,
        &store,
        &bus,
        &run_id,
        engine,
        model,
        permission_mode,
    );
}
