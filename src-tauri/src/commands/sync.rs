//! Two-way sync commands (design §3, §7), used by Settings → Sync.
//!
//! Group secrets live in the Keychain (`secrets::SyncSecrets`). The pause and
//! "sync secrets" flags are machine-local `settings` keys (`sync_paused`,
//! `sync_secrets_enabled`) and must never be synced themselves. Device names
//! and the leader device are synced settings (`sync::leader`).

use crate::db::Db;
use crate::sync::engine::{StatusSink, SyncEngine, SyncStatus, Timing, STATUS_EVENT};
use crate::sync::leader::{self, DeviceInfo};
use crate::sync::store::{self, ParkedRow, SqliteSyncStore, SyncStore};
use crate::sync::transport::{Connection, TransportError};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Mutex;

const KEY_PAUSED: &str = "sync_paused";
const KEY_SECRETS: &str = crate::sync::secrets::SETTING;

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
        let paths = crate::sync::codec::PathCtx::from_env()
            .with_app_data(app.path().app_data_dir().map_err(|e| format!("no app data dir: {e}"))?);
        let store: Arc<dyn SyncStore> = Arc::new(
            SqliteSyncStore::new(db.clone(), sync.device_id.clone())
                .with_paths(paths)
                .with_secrets(Arc::new(crate::sync::secrets::KeychainBackend))
                .with_notifier(crate::sync::events::app_notifier(app.clone())),
        );
        let paused = crate::remote::get_setting(db, KEY_PAUSED).await.as_deref() == Some("true");
        let emitter = app.clone();
        let sink: StatusSink = Arc::new(move |s: &SyncStatus| {
            let _ = emitter.emit(STATUS_EVENT, s);
        });
        *slot = Some(SyncEngine::start(params, keys, store, sink, paused, Timing::default()));
        leader::set_sync_active(true);
        let name = default_device_name(db).await;
        if let Err(e) = leader::touch_self(db, &sync.device_id, &name).await {
            tracing::warn!(event = "sync_device_record_failed", error = %e);
        }
        Ok(())
    }

    pub async fn stop(&self) {
        if let Some(engine) = self.engine.lock().await.take() {
            engine.stop().await;
        }
        leader::set_sync_active(false);
    }

    /// The outbox changed: sync soon. Cheap; safe to call from anywhere.
    pub async fn kick(&self) {
        if let Some(e) = self.engine.lock().await.as_ref() {
            e.kick();
        }
    }

    /// Push what is queued now, waiting at most `wait` (best effort: offline
    /// just times out).
    async fn flush(&self, wait: Duration) {
        let started = chrono::Utc::now().timestamp_millis();
        match self.engine.lock().await.as_ref() {
            Some(e) => e.sync_now(),
            None => return,
        }
        let deadline = tokio::time::Instant::now() + wait;
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let s = self.status().await;
            if s.pending == 0 && s.last_push.is_some_and(|p| p >= started) {
                break;
            }
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
    let sync = crate::sync::pair::create_group(db.inner()).await?;
    crate::sync::bootstrap::prepare_new_group(db.inner()).await?;
    // The creator leads singleton jobs until someone picks another device.
    leader::claim_group(db.inner(), &sync.device_id).await?;
    state.start(&app, db.inner()).await?;
    Ok(state.status().await)
}

/// Whether this machine has its own relay URL + token (Settings → Remote
/// Control), which creating or joining a group needs.
#[tauri::command]
pub async fn sync_relay_ready(db: State<'_, Db>) -> Result<bool, String> {
    Ok(crate::sync::pair::relay_ready(db.inner()).await)
}

/// Join the group of the device named `device_alias` with its Remote Control
/// master password. That device's user must have opened "Accept a new
/// device" and approve the request, which shows this device's name.
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
    if !crate::sync::pair::relay_ready(db.inner()).await {
        return Err("relay_not_configured: set the relay URL and token in Settings → Remote Control first".into());
    }
    let name = default_device_name(db.inner()).await;
    // Leaves the merge pending: the engine starts after sync_bootstrap_commit.
    crate::sync::pair::join(db.inner(), alias, password, &name).await?;
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
    // Hand the leader role on and drop this device's record while the engine
    // can still push it.
    if let Some(sync) = crate::secrets::get_sync_secrets() {
        if state.engine.lock().await.is_some() {
            match leader::on_leave(db.inner(), &sync.device_id).await {
                Ok(()) => state.flush(Duration::from_secs(5)).await,
                Err(e) => tracing::warn!(event = "sync_leave_handover_failed", error = %e),
            }
        }
    }
    crate::sync::pair::close_accept_window();
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

/// Whether Keychain secrets are synced (D4: default on when never set).
#[tauri::command]
pub async fn sync_get_secrets(db: State<'_, Db>) -> Result<bool, String> {
    Ok(crate::remote::get_setting(db.inner(), KEY_SECRETS).await.as_deref() != Some("false"))
}

/// Whether Keychain secrets (and managed SSH key files) are synced (D4,
/// default on).
#[tauri::command]
pub async fn sync_set_secrets(db: State<'_, Db>, enabled: bool) -> Result<(), String> {
    crate::remote::set_setting(db.inner(), KEY_SECRETS, if enabled { "true" } else { "false" }).await
}

/// Name a device gets until the user renames it: the Remote Control alias,
/// else the computer name.
async fn default_device_name(db: &Db) -> String {
    if let Some(alias) = crate::remote::device_alias(db).await {
        return alias;
    }
    let computer = tokio::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .await
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|n| !n.is_empty());
    computer.unwrap_or_else(|| "Devdy".to_string())
}

fn group_device_id() -> Result<String, String> {
    crate::secrets::get_sync_secrets()
        .map(|s| s.device_id)
        .ok_or_else(|| "this device is not in a sync group".to_string())
}

/// Devices of the group (from their synced name records), this one first.
#[tauri::command]
pub async fn sync_devices_list(db: State<'_, Db>) -> Result<Vec<DeviceInfo>, String> {
    let me = group_device_id()?;
    leader::list_devices(db.inner(), &me).await
}

/// Rename this device (each device names only itself).
#[tauri::command]
pub async fn sync_device_rename(
    db: State<'_, Db>,
    state: State<'_, SyncManager>,
    name: String,
) -> Result<(), String> {
    let me = group_device_id()?;
    leader::rename_self(db.inner(), &me, &name).await?;
    state.kick().await;
    Ok(())
}

const ONLY_CREATOR: &str = "only the device that created the group can remove other devices";

/// Remove another device from the group: revoke its token on the relay, then
/// drop its record (the leader role passes to this device if it held it).
/// Only the group creator may do this (the relay enforces it). The removed
/// device keeps the group key but can no longer reach the relay's data.
#[tauri::command]
pub async fn sync_revoke_device(
    db: State<'_, Db>,
    state: State<'_, SyncManager>,
    device_id: String,
) -> Result<(), String> {
    let sync = crate::secrets::get_sync_secrets().ok_or("this device is not in a sync group")?;
    if device_id == sync.device_id {
        return Err("use Leave group to remove this device".into());
    }
    if leader::creator(db.inner()).await.is_some_and(|c| c != sync.device_id) {
        return Err(ONLY_CREATOR.into());
    }
    let params = crate::sync::pair::connect_params(db.inner(), &sync).await?;
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let conn = Connection::connect(&params, false, tx).await.map_err(|e| match e {
        TransportError::Rejected(_) => e.to_string(),
        other => format!("relay unreachable: {other}"),
    })?;
    conn.revoke(&device_id).await.map_err(|e| match e {
        TransportError::Rejected(c) | TransportError::Relay(c) if c == "forbidden" => ONLY_CREATOR.to_string(),
        other => other.to_string(),
    })?;
    drop(conn);
    leader::forget_device(db.inner(), &device_id, &sync.device_id).await?;
    tracing::info!(event = "sync_device_revoked");
    state.kick().await;
    Ok(())
}

/// Pick the device that runs singleton jobs (Pulse / Slack polling); `None`
/// lets every device run them. Takes effect on the next poll, no restart.
#[tauri::command]
pub async fn sync_set_leader(
    db: State<'_, Db>,
    state: State<'_, SyncManager>,
    device_id: Option<String>,
) -> Result<(), String> {
    leader::set_leader(db.inner(), device_id.as_deref()).await?;
    state.kick().await;
    Ok(())
}

/// Open "Accept a new device" for `sync::pair::ACCEPT_WINDOW`; returns the
/// expiry (unix millis). Join requests arriving meanwhile still need the
/// user's approval (`sync://join-request` → `sync_join_respond`).
#[tauri::command]
pub async fn sync_accept_open(app: AppHandle) -> Result<i64, String> {
    if crate::secrets::get_sync_secrets().is_none() {
        return Err("this device is not in a sync group".into());
    }
    let expires = crate::sync::pair::open_accept_window();
    let _ = app.emit(crate::sync::pair::ACCEPT_WINDOW_EVENT, json!({ "expires_at": expires }));
    Ok(expires)
}

/// Close the window early (a request still waiting is denied).
#[tauri::command]
pub async fn sync_accept_close(app: AppHandle) -> Result<(), String> {
    crate::sync::pair::close_accept_window();
    let _ = app.emit(crate::sync::pair::ACCEPT_WINDOW_EVENT, json!({ "expires_at": null }));
    Ok(())
}

/// The open window's expiry (unix millis), if any.
#[tauri::command]
pub async fn sync_accept_state() -> Result<Option<i64>, String> {
    Ok(crate::sync::pair::accept_window_expires())
}

/// Answer a pending join request.
#[tauri::command]
pub async fn sync_join_respond(request_id: String, allow: bool) -> Result<(), String> {
    crate::sync::pair::respond_join(&request_id, allow)
}

/// How many parked rows `sync_parked_list` returns at most.
const PARKED_LIST_LIMIT: usize = 200;

/// Rows whose latest change the relay permanently refused (newest first).
#[tauri::command]
pub async fn sync_parked_list(db: State<'_, Db>) -> Result<Vec<ParkedRow>, String> {
    store::parked_list(db.inner(), PARKED_LIST_LIMIT).await
}

/// Queue the parked rows for push again; returns how many. Rows the relay
/// still refuses are parked again.
#[tauri::command]
pub async fn sync_parked_retry(db: State<'_, Db>, state: State<'_, SyncManager>) -> Result<usize, String> {
    let n = store::parked_retry(db.inner()).await?;
    if let Some(e) = state.engine.lock().await.as_ref() {
        e.set_parked(0);
        e.sync_now();
    }
    Ok(n)
}
