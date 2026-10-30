//! Leader device for singleton jobs (design §0 D6) and the device list of the
//! group (§7).
//!
//! Both live in synced `settings` rows so every device sees the same answer:
//! - `sync_leader_device` = the device id that runs Pulse polling and Slack
//!   recovery. Unset = every device runs them (as before sync).
//! - `sync_device_name:<device id>` = `{"name", "last_seen"}`, one row per
//!   device, written only by that device (so concurrent edits never collide).
//!   The settings filter accepts the prefix (`schema::SYNCED_SETTING_PREFIXES`).
//!
//! Whether this device is in a group comes from the running engine
//! (`set_sync_active`), not the Keychain, so the check is cheap enough for a
//! 3 s poller.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::db::Db;

pub const KEY_LEADER: &str = "sync_leader_device";
pub const DEVICE_NAME_PREFIX: &str = "sync_device_name:";

/// Refresh this device's `last_seen` at most this often (each write is an op).
const SEEN_REFRESH_MS: i64 = 60 * 60 * 1000;
/// Leader changes arriving through sync are noticed within this delay.
const WATCH_INTERVAL: Duration = Duration::from_secs(30);

static SYNC_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Set once the startup sync autostart finished (in a group or not), so the
/// first leader decision does not run before the engine had a chance to start.
static SYNC_READY: AtomicBool = AtomicBool::new(false);

pub fn set_sync_active(active: bool) {
    SYNC_ACTIVE.store(active, Ordering::SeqCst);
}

pub fn mark_ready() {
    SYNC_READY.store(true, Ordering::SeqCst);
}

/// Whether this device should run singleton jobs.
pub fn decide(sync_on: bool, leader: Option<&str>, me: &str) -> bool {
    match leader.map(str::trim).filter(|l| !l.is_empty()) {
        _ if !sync_on => true,
        None => true,
        Some(l) => l == me,
    }
}

async fn setting(db: &Db, key: &str) -> Option<String> {
    sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
}

async fn put_setting(db: &Db, key: &str, value: &str) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES (?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(db)
    .await
    .map(drop)
    .map_err(|e| e.to_string())
}

async fn delete_setting(db: &Db, key: &str) -> Result<(), String> {
    sqlx::query("DELETE FROM settings WHERE key = ?")
        .bind(key)
        .execute(db)
        .await
        .map(drop)
        .map_err(|e| e.to_string())
}

/// This machine's sync device id (`sync_device`), if created yet.
pub async fn my_device_id(db: &Db) -> Option<String> {
    sqlx::query_scalar("SELECT id FROM sync_device LIMIT 1")
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
}

pub async fn leader(db: &Db) -> Option<String> {
    setting(db, KEY_LEADER).await.filter(|l| !l.trim().is_empty())
}

/// `is_leader` with an explicit "in a sync group" flag (tests).
pub async fn is_leader_with(db: &Db, sync_on: bool) -> bool {
    if !sync_on {
        return true;
    }
    let Some(leader) = leader(db).await else { return true };
    let Some(me) = my_device_id(db).await else { return true };
    decide(true, Some(&leader), &me)
}

/// Whether this device runs singleton jobs right now.
pub async fn is_leader(db: &Db) -> bool {
    is_leader_with(db, SYNC_ACTIVE.load(Ordering::SeqCst)).await
}

/// `None` clears the leader (every device runs singleton jobs).
pub async fn set_leader(db: &Db, device_id: Option<&str>) -> Result<(), String> {
    match device_id.map(str::trim).filter(|d| !d.is_empty()) {
        Some(id) => put_setting(db, KEY_LEADER, id).await,
        None => delete_setting(db, KEY_LEADER).await,
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DeviceRecord {
    #[serde(default)]
    pub name: String,
    /// Unix millis.
    #[serde(default)]
    pub last_seen: Option<i64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub last_seen: Option<i64>,
    pub is_self: bool,
    pub is_leader: bool,
}

pub fn parse_record(value: &str) -> DeviceRecord {
    serde_json::from_str(value).unwrap_or_default()
}

fn name_key(device_id: &str) -> String {
    format!("{DEVICE_NAME_PREFIX}{device_id}")
}

pub async fn record(db: &Db, device_id: &str) -> Option<DeviceRecord> {
    setting(db, &name_key(device_id)).await.map(|v| parse_record(&v))
}

async fn write_record(db: &Db, device_id: &str, rec: &DeviceRecord) -> Result<(), String> {
    let value = json!({ "name": rec.name, "last_seen": rec.last_seen }).to_string();
    put_setting(db, &name_key(device_id), &value).await
}

/// Every device known to the group (this one first), sorted by name.
pub async fn list_devices(db: &Db, me: &str) -> Result<Vec<DeviceInfo>, String> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT key, value FROM settings WHERE substr(key, 1, ?) = ?")
            .bind(DEVICE_NAME_PREFIX.len() as i64)
            .bind(DEVICE_NAME_PREFIX)
            .fetch_all(db)
            .await
            .map_err(|e| e.to_string())?;
    let leader = leader(db).await;
    let mut out: Vec<DeviceInfo> = rows
        .into_iter()
        .filter_map(|(k, v)| {
            let id = k.strip_prefix(DEVICE_NAME_PREFIX)?.to_string();
            let rec = parse_record(&v);
            Some(DeviceInfo {
                is_self: id == me,
                is_leader: leader.as_deref() == Some(id.as_str()),
                id,
                name: rec.name,
                last_seen: rec.last_seen,
            })
        })
        .collect();
    if !out.iter().any(|d| d.is_self) {
        out.push(DeviceInfo {
            id: me.to_string(),
            name: String::new(),
            last_seen: None,
            is_self: true,
            is_leader: leader.as_deref() == Some(me),
        });
    }
    out.sort_by(|a, b| {
        b.is_self
            .cmp(&a.is_self)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(out)
}

/// Publish this device's record: keep its name (or `default_name` the first
/// time) and refresh `last_seen` when stale.
pub async fn touch_self(db: &Db, me: &str, default_name: &str) -> Result<(), String> {
    let now = chrono::Utc::now().timestamp_millis();
    let existing = record(db, me).await;
    let fresh = existing
        .as_ref()
        .and_then(|r| r.last_seen)
        .is_some_and(|seen| now - seen < SEEN_REFRESH_MS);
    if fresh && existing.as_ref().is_some_and(|r| !r.name.is_empty()) {
        return Ok(());
    }
    let name = existing
        .map(|r| r.name)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| default_name.to_string());
    write_record(db, me, &DeviceRecord { name, last_seen: Some(now) }).await
}

pub async fn rename_self(db: &Db, me: &str, name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err("device name must be 1–64 characters".into());
    }
    let now = chrono::Utc::now().timestamp_millis();
    write_record(db, me, &DeviceRecord { name: name.to_string(), last_seen: Some(now) }).await
}

/// Forget a removed device: its record, and the leader role if it held it.
pub async fn forget_device(db: &Db, device_id: &str) -> Result<(), String> {
    delete_setting(db, &name_key(device_id)).await?;
    if leader(db).await.as_deref() == Some(device_id) {
        delete_setting(db, KEY_LEADER).await?;
    }
    Ok(())
}

/// Run `on_gain` whenever this device becomes the leader — once at startup
/// (after the sync autostart settled) and on every later switch, whether it
/// was made here or arrived through sync.
pub fn watch_leadership<F, Fut>(db: Db, on_gain: F)
where
    F: Fn() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    tauri::async_runtime::spawn(async move {
        // The autostart reads the Keychain; don't wait for it forever.
        for _ in 0..240 {
            if SYNC_READY.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let mut was = false;
        loop {
            let now = is_leader(&db).await;
            if now && !was {
                on_gain().await;
            }
            was = now;
            tokio::time::sleep(WATCH_INTERVAL).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_leader() {
        assert!(decide(false, Some("other"), "me"), "sync off: always run");
        assert!(decide(true, None, "me"), "no leader: everyone runs");
        assert!(decide(true, Some("  "), "me"));
        assert!(decide(true, Some("me"), "me"));
        assert!(!decide(true, Some("other"), "me"));
    }

    #[test]
    fn parses_records_leniently() {
        assert_eq!(
            parse_record(r#"{"name":"Mac mini","last_seen":5}"#),
            DeviceRecord { name: "Mac mini".into(), last_seen: Some(5) }
        );
        assert_eq!(parse_record("garbage"), DeviceRecord::default());
    }

    async fn db() -> Db {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        crate::sync::install(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn leader_follows_the_setting() {
        let db = db().await;
        let me = my_device_id(&db).await.unwrap();
        assert!(is_leader_with(&db, true).await);
        set_leader(&db, Some("someone-else")).await.unwrap();
        assert!(!is_leader_with(&db, true).await);
        assert!(is_leader_with(&db, false).await, "sync off ignores the leader");
        set_leader(&db, Some(&me)).await.unwrap();
        assert!(is_leader_with(&db, true).await);
        set_leader(&db, None).await.unwrap();
        assert!(is_leader_with(&db, true).await);
    }

    #[tokio::test]
    async fn device_records_list_rename_forget() {
        let db = db().await;
        touch_self(&db, "me", "MacBook").await.unwrap();
        write_record(&db, "mini", &DeviceRecord { name: "Mac mini".into(), last_seen: Some(1) }).await.unwrap();
        set_leader(&db, Some("mini")).await.unwrap();

        let list = list_devices(&db, "me").await.unwrap();
        assert_eq!(list.len(), 2);
        assert!(list[0].is_self && list[0].name == "MacBook");
        assert!(list[1].is_leader && list[1].name == "Mac mini");

        rename_self(&db, "me", "  Air ").await.unwrap();
        assert_eq!(record(&db, "me").await.unwrap().name, "Air");
        assert!(rename_self(&db, "me", "   ").await.is_err());
        // A later touch keeps the chosen name.
        touch_self(&db, "me", "MacBook").await.unwrap();
        assert_eq!(record(&db, "me").await.unwrap().name, "Air");

        forget_device(&db, "mini").await.unwrap();
        assert!(leader(&db).await.is_none(), "removing the leader clears the role");
        assert_eq!(list_devices(&db, "me").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn device_names_and_leader_are_captured_for_sync() {
        let db = db().await;
        touch_self(&db, "me", "MacBook").await.unwrap();
        set_leader(&db, Some("me")).await.unwrap();
        let keys: Vec<String> = sqlx::query_scalar("SELECT pk FROM sync_outbox WHERE tbl = 'settings'")
            .fetch_all(&db)
            .await
            .unwrap();
        assert!(keys.iter().any(|k| k.contains("sync_device_name:me")), "{keys:?}");
        assert!(keys.iter().any(|k| k.contains(KEY_LEADER)), "{keys:?}");
    }
}
