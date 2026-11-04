//! Tauri commands of the pulling side. Local-only: a remote controller may not
//! call them (`remote::guard::LOCAL_ONLY_COMMANDS`).

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Emitter, State};

use crate::db::Db;
use crate::device_sync::bundle::{apply_sync_bundle, SyncCategories, SyncReport};
use crate::device_sync::transport::{self, OnlineDevice, Phase, Progress, Puller};
use crate::remote;

/// Event carrying [`Progress`] during a pull.
pub const PROGRESS_EVENT: &str = "device-sync-progress";

/// This device's relay URL, host token and device id; B needs no running
/// Remote Control agent, only its configuration.
async fn relay_identity(db: &Db) -> Result<(String, String, String), String> {
    let url = remote::get_setting(db, remote::KEY_RELAY_URL)
        .await
        .ok_or_else(|| "relay_not_configured".to_string())?;
    let token = crate::secrets::get_remote_auth_token()
        .ok_or_else(|| "relay_not_configured".to_string())?;
    let device_id = remote::ensure_device_id(db).await?;
    Ok((url, token, device_id))
}

/// Other Devdy desktops of the owner online on the relay.
#[tauri::command]
pub async fn device_sync_list_devices(db: State<'_, Db>) -> Result<Vec<OnlineDevice>, String> {
    let (url, token, device_id) = relay_identity(db.inner()).await?;
    transport::list_devices(&url, &token, &device_id).await
}

/// One pull at a time on this device.
static PULLING: AtomicBool = AtomicBool::new(false);

struct PullGuard;

impl Drop for PullGuard {
    fn drop(&mut self) {
        PULLING.store(false, Ordering::SeqCst);
    }
}

/// Pull `categories` from `device_id` and apply them here. `password` is the
/// source device's master password; it never leaves this machine.
#[tauri::command]
pub async fn device_sync_pull(
    app: AppHandle,
    db: State<'_, Db>,
    device_id: String,
    categories: SyncCategories,
    password: String,
) -> Result<SyncReport, String> {
    if password.is_empty() {
        return Err(transport::ERR_WRONG_PASSWORD.to_string());
    }
    if PULLING.swap(true, Ordering::SeqCst) {
        return Err("busy".to_string());
    }
    let _guard = PullGuard;
    let (url, _token, my_id) = relay_identity(db.inner()).await?;
    if device_id == my_id {
        return Err(transport::ERR_DEVICE_OFFLINE.to_string());
    }
    let emit = |p: Progress| {
        let _ = app.emit(PROGRESS_EVENT, p);
    };
    let psk = tokio::task::spawn_blocking(move || remote::login::derive_login_psk(&password))
        .await
        .map_err(|_| transport::ERR_PROTOCOL.to_string())?;
    let puller = Puller {
        device_id: my_id,
        alias: remote::device_alias(db.inner()).await.unwrap_or_default(),
    };
    let pulled = transport::pull(&url, &device_id, &puller, &categories, &psk, &emit).await?;
    let total = pulled.bytes.len() as u64;
    emit(Progress {
        phase: Phase::Applying,
        received: total,
        total,
    });
    let report =
        apply_sync_bundle(&app, db.inner(), pulled.bytes, &pulled.key, &categories).await?;
    emit(Progress {
        phase: Phase::Done,
        received: total,
        total,
    });
    tracing::info!(event = "device_sync_pulled", bytes = total);
    Ok(report)
}
