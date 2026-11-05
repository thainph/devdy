//! Tauri commands of the pulling side. Only the local UI can call them: the web
//! controller has no channel to invoke Tauri commands on the host.

use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use crate::db::Db;
use crate::device_sync::bundle::{apply_sync_bundle, SyncCategories, SyncReport};
use crate::device_sync::transport::{self, OnlineDevice, Phase, Progress, Puller};
use crate::remote;

/// Event carrying [`Progress`] during a pull.
pub const PROGRESS_EVENT: &str = "device-sync-progress";

/// Setting holding the last successful pull ([`LastSync`] JSON). Local to this
/// machine: it is not a portable setting, so backups and syncs never carry it.
pub const KEY_LAST_SYNC: &str = "device_sync_last";

/// The last successful pull on this device, shown in Settings → Devices and the
/// sidebar Remote popover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastSync {
    /// RFC 3339 time the pull finished.
    pub at: String,
    pub source_device_id: String,
    /// The source's name as shown in the device list ("" when it had none).
    pub source_alias: String,
    /// Categories ticked (mirror not counted).
    pub categories: u32,
    pub created: u32,
    pub updated: u32,
    pub deleted: u32,
}

impl LastSync {
    /// Summary of a pull from `source_device_id` that produced `report`.
    pub fn new(
        at: chrono::DateTime<chrono::Utc>,
        source_device_id: &str,
        source_alias: &str,
        categories: &SyncCategories,
        report: &SyncReport,
    ) -> Self {
        let c = categories;
        let ticked = [
            c.todos,
            c.notes,
            c.secret_notes,
            c.slack_threads,
            c.skills_rules,
            c.projects,
            c.settings,
            c.mcp_servers,
            c.vps_servers,
            c.accounts,
        ];
        let sum = |f: fn(&crate::device_sync::bundle::SyncCounts) -> u32| {
            report.counts.values().map(f).sum::<u32>()
        };
        let alias = source_alias.trim().to_lowercase();
        Self {
            at: at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            source_device_id: source_device_id.to_string(),
            source_alias: if remote::login::valid_alias(&alias) {
                alias
            } else {
                String::new()
            },
            categories: ticked.iter().filter(|t| **t).count() as u32,
            created: sum(|c| c.created),
            updated: sum(|c| c.updated),
            deleted: sum(|c| c.deleted),
        }
    }
}

/// The last successful pull on this device, if any.
#[tauri::command]
pub async fn device_sync_last(db: State<'_, Db>) -> Result<Option<LastSync>, String> {
    Ok(remote::get_setting(db.inner(), KEY_LAST_SYNC)
        .await
        .and_then(|v| serde_json::from_str(&v).ok()))
}

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
    source_alias: Option<String>,
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
    let last = LastSync::new(
        chrono::Utc::now(),
        &device_id,
        source_alias.as_deref().unwrap_or_default(),
        &categories,
        &report,
    );
    if let Ok(json) = serde_json::to_string(&last) {
        // Only a display hint: a failed write must not fail the pull.
        if let Err(e) = remote::set_setting(db.inner(), KEY_LAST_SYNC, &json).await {
            tracing::warn!(event = "device_sync_last_write_failed", error = %e);
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_sync::bundle::SyncCounts;

    #[test]
    fn last_sync_sums_the_report_and_counts_ticked_categories() {
        let categories = SyncCategories {
            todos: true,
            notes: true,
            settings: true,
            mirror: true,
            ..Default::default()
        };
        let mut report = SyncReport::default();
        report.counts.insert(
            "todos".into(),
            SyncCounts {
                created: 3,
                updated: 14,
                unchanged: 2,
                deleted: 1,
                skipped: 0,
            },
        );
        report.counts.insert(
            "notes".into(),
            SyncCounts {
                created: 1,
                updated: 6,
                unchanged: 0,
                deleted: 0,
                skipped: 4,
            },
        );
        let at = chrono::DateTime::parse_from_rfc3339("2026-11-04T10:42:07Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let last = LastSync::new(at, "dev-b", " Mac-Mini ", &categories, &report);
        assert_eq!(
            last,
            LastSync {
                at: "2026-11-04T10:42:07Z".into(),
                source_device_id: "dev-b".into(),
                source_alias: "mac-mini".into(),
                categories: 3,
                created: 4,
                updated: 20,
                deleted: 1,
            }
        );
        let json = serde_json::to_value(&last).unwrap();
        assert_eq!(json["sourceAlias"], "mac-mini");
        assert_eq!(json["sourceDeviceId"], "dev-b");
    }

    #[test]
    fn last_sync_drops_an_invalid_alias() {
        let last = LastSync::new(
            chrono::Utc::now(),
            "dev-b",
            "not a valid name!",
            &SyncCategories::default(),
            &SyncReport::default(),
        );
        assert_eq!(last.source_alias, "");
        assert_eq!(last.categories, 0);
    }
}
