//! Two-way sync commands (design §3, §7). No UI in wave 1.
//!
//! Group secrets live in the Keychain (`secrets::SyncSecrets`). The pause and
//! "sync secrets" flags are machine-local `settings` keys (`sync_paused`,
//! `sync_secrets_enabled`) and must never be synced themselves.

use crate::db::Db;
use crate::sync::engine::{StatusSink, SyncEngine, SyncStatus, Timing, STATUS_EVENT};
use crate::sync::store::{SqliteSyncStore, SyncStore};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Mutex;

const KEY_PAUSED: &str = "sync_paused";
const KEY_SECRETS: &str = "sync_secrets_enabled";

/// App-wide sync state (Tauri managed): the running engine, if any.
#[derive(Clone, Default)]
pub struct SyncManager {
    engine: Arc<Mutex<Option<SyncEngine>>>,
}

impl SyncManager {
    /// Start the engine when this device is in a group (no-op otherwise or
    /// when already running).
    pub async fn start(&self, app: &AppHandle, db: &Db) -> Result<(), String> {
        let mut slot = self.engine.lock().await;
        if slot.is_some() {
            return Ok(());
        }
        let Some(sync) = crate::secrets::get_sync_secrets() else {
            return Ok(());
        };
        // Joined but not merged yet: pushing now would send local ids.
        if crate::sync::bootstrap::is_pending(db).await? {
            return Ok(());
        }
        let params = crate::sync::pair::connect_params(db, &sync).await?;
        let keys = crate::sync::pair::keys_of(&sync)?;
        let store: Arc<dyn SyncStore> = Arc::new(SqliteSyncStore::new(db.clone(), sync.device_id.clone()));
        let paused = crate::remote::get_setting(db, KEY_PAUSED).await.as_deref() == Some("true");
        let emitter = app.clone();
        let sink: StatusSink = Arc::new(move |s: &SyncStatus| {
            let _ = emitter.emit(STATUS_EVENT, s);
        });
        *slot = Some(SyncEngine::start(params, keys, store, sink, paused, Timing::default()));
        Ok(())
    }

    pub async fn stop(&self) {
        if let Some(engine) = self.engine.lock().await.take() {
            engine.stop().await;
        }
    }

    /// The outbox changed: sync soon. Cheap; safe to call from anywhere.
    pub async fn kick(&self) {
        if let Some(e) = self.engine.lock().await.as_ref() {
            e.kick();
        }
    }

    async fn status(&self) -> SyncStatus {
        match self.engine.lock().await.as_ref() {
            Some(e) => e.status(),
            None => SyncStatus::disabled(),
        }
    }
}

fn emit_disabled(app: &AppHandle) {
    let _ = app.emit(STATUS_EVENT, SyncStatus::disabled());
}

#[tauri::command]
pub async fn sync_status(state: State<'_, SyncManager>) -> Result<SyncStatus, String> {
    Ok(state.status().await)
}

/// First device: create a new group on the relay and start syncing.
#[tauri::command]
pub async fn sync_create_group(
    app: AppHandle,
    db: State<'_, Db>,
    state: State<'_, SyncManager>,
) -> Result<SyncStatus, String> {
    if crate::secrets::get_sync_secrets().is_some() {
        return Err("this device is already in a sync group".into());
    }
    crate::sync::pair::create_group(db.inner()).await?;
    crate::sync::bootstrap::prepare_new_group(db.inner()).await?;
    state.start(&app, db.inner()).await?;
    Ok(state.status().await)
}

/// Join the group of the device named `device_alias` with its Remote Control
/// master password.
#[tauri::command]
pub async fn sync_join(
    db: State<'_, Db>,
    state: State<'_, SyncManager>,
    device_alias: String,
    password: String,
) -> Result<SyncStatus, String> {
    if crate::secrets::get_sync_secrets().is_some() {
        return Err("this device is already in a sync group".into());
    }
    let alias = device_alias.trim();
    if !crate::remote::login::valid_alias(alias) {
        return Err("invalid device alias".into());
    }
    if password.is_empty() {
        return Err("password is required".into());
    }
    crate::sync::pair::join(db.inner(), alias, password).await?;
    // The engine starts after sync_bootstrap_commit.
    crate::sync::bootstrap::mark_joined(db.inner()).await?;
    Ok(state.status().await)
}

/// Leave the group: revoke this device on the relay and wipe the Keychain
/// entry. Returns whether the relay confirmed the revoke (false = offline;
/// the local membership is gone either way).
#[tauri::command]
pub async fn sync_leave(
    app: AppHandle,
    db: State<'_, Db>,
    state: State<'_, SyncManager>,
) -> Result<bool, String> {
    state.stop().await;
    let revoked = crate::sync::pair::leave(db.inner()).await?;
    emit_disabled(&app);
    Ok(revoked)
}

#[tauri::command]
pub async fn sync_pause(
    db: State<'_, Db>,
    state: State<'_, SyncManager>,
    paused: bool,
) -> Result<SyncStatus, String> {
    crate::remote::set_setting(db.inner(), KEY_PAUSED, if paused { "true" } else { "false" }).await?;
    if let Some(e) = state.engine.lock().await.as_ref() {
        e.set_paused(paused);
    }
    Ok(state.status().await)
}

#[tauri::command]
pub async fn sync_now(state: State<'_, SyncManager>) -> Result<(), String> {
    match state.engine.lock().await.as_ref() {
        Some(e) => {
            e.sync_now();
            Ok(())
        }
        None => Err("this device is not in a sync group".into()),
    }
}

/// Whether Keychain secrets are synced (D4, default on). Stored only for now;
/// secrets sync lands in wave 2.
#[tauri::command]
pub async fn sync_set_secrets(db: State<'_, Db>, enabled: bool) -> Result<(), String> {
    crate::remote::set_setting(db.inner(), KEY_SECRETS, if enabled { "true" } else { "false" }).await
}
