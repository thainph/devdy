//! Leader device for singleton jobs (design §0 D6) and the device list of the
//! group (§7).
//!
//! Both live in synced `settings` rows so every device sees the same answer:
//! - `sync_leader_device` = the device id that runs Slack
//!   recovery. Unset = every device runs them (as before sync).
//! - `sync_device_name:<device id>` = `{"name", "last_seen"}`, one row per
//!   device, written only by that device (so concurrent edits never collide).
//!   The settings filter accepts the prefix (`schema::SYNCED_SETTING_PREFIXES`).
//! - `sync_group_creator` = the device that may remove others (the relay
//!   enforces it; the UI reads this to explain it). It starts as the device
//!   that created the group; when that device leaves, the relay moves the role
//!   to the successor it names (or the oldest remaining member) and tells
//!   every device (`sync_welcome` / `sync_group_info` → [`set_creator`]). The
//!   creator starts as leader, and removing / losing the leader hands the role
//!   on (remover, creator, or the leaver's successor) so singleton jobs never
//!   stop silently.
//!
//! Leader hand-overs are decided by one device only — the remover, or the
//! leaving leader itself — and the only automatic claim ([`heal_leader`]) is
//! made by the relay-designated creator, so two devices never both take the
//! role.
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
pub const KEY_CREATOR: &str = "sync_group_creator";

/// Refresh this device's `last_seen` at most this often (each write is an op).
const SEEN_REFRESH_MS: i64 = 60 * 60 * 1000;
/// Leader changes arriving through sync are noticed within this delay.
const WATCH_INTERVAL: Duration = Duration::from_secs(30);

static SYNC_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Machine-local (`sync_state`, never synced): the leader this device last
/// saw, so it notices the role leaving it — set here or arrived through sync.
const SEEN_KEY: &str = "leader_seen";
/// Machine-local: the device this one handed the leader role to and which has
/// not confirmed it synced that yet (on-demand sync: it learns only when it
/// syncs). Until then this device keeps running the singleton jobs and keeps
/// telling it (`leader_changed`), across restarts.
const HANDOFF_KEY: &str = "leader_handoff";

async fn state(db: &Db, key: &str) -> Option<String> {
    sqlx::query_scalar("SELECT value FROM sync_state WHERE key = ?")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .filter(|v: &String| !v.is_empty())
}

async fn put_state(db: &Db, key: &str, value: Option<&str>) -> Result<(), String> {
    let q = match value {
        Some(v) => sqlx::query("INSERT INTO sync_state (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
            .bind(key)
            .bind(v.to_string()),
        None => sqlx::query("DELETE FROM sync_state WHERE key = ?").bind(key),
    };
    q.execute(db).await.map(drop).map_err(|e| e.to_string())
}

/// The unconfirmed hand-off of the leader role, if any.
pub async fn handoff_pending(db: &Db) -> Option<String> {
    state(db, HANDOFF_KEY).await
}

/// Forget the hand-off (confirmed, or this device leaves the group).
pub async fn clear_handoff(db: &Db) -> Result<(), String> {
    put_state(db, HANDOFF_KEY, None).await
}

/// This device leaves the group: forget what it saw of the leader role.
pub async fn forget_local(db: &Db) -> Result<(), String> {
    put_state(db, HANDOFF_KEY, None).await?;
    put_state(db, SEEN_KEY, None).await
}

/// `to` confirmed: clear the hand-off if it is still the one pending.
pub async fn handoff_confirmed(db: &Db, to: &str) -> Result<(), String> {
    if handoff_pending(db).await.as_deref() == Some(to) {
        clear_handoff(db).await?;
    }
    Ok(())
}

/// The old leader keeps the jobs while its hand-off to the current leader is
/// unconfirmed (a later change made elsewhere ends that).
pub fn keeps_jobs(handoff: Option<&str>, leader: Option<&str>) -> bool {
    handoff.is_some() && handoff == leader
}

/// Notice the leader role moving: when it left this device (whoever made the
/// change), record a hand-off to the new leader. Returns the pending hand-off.
pub async fn observe_change(db: &Db, me: &str) -> Result<Option<String>, String> {
    let now = leader(db).await;
    let seen = state(db, SEEN_KEY).await;
    if seen.as_deref() != now.as_deref() {
        // No leader means every device runs the jobs, so this one held the role too.
        let held_here = seen.as_deref().is_none_or(|s| s == me);
        if let (true, Some(new)) = (held_here, now.as_deref().filter(|n| *n != me)) {
            put_state(db, HANDOFF_KEY, Some(new)).await?;
        }
        put_state(db, SEEN_KEY, now.as_deref()).await?;
    }
    let pending = handoff_pending(db).await;
    if pending.is_some() && !keeps_jobs(pending.as_deref(), now.as_deref()) {
        // The role moved on again (or back here): nothing left to hand over.
        clear_handoff(db).await?;
        return Ok(None);
    }
    Ok(pending)
}

/// One round of an unconfirmed hand-off: notice changes, then tell the new
/// leader (`notify`: sends `leader_changed` and waits for it to sync). Returns
/// whether a hand-off is still pending.
pub async fn handoff_step<F, Fut>(db: &Db, me: &str, notify: F) -> Result<bool, String>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let Some(to) = observe_change(db, me).await? else { return Ok(false) };
    match notify(to.clone()).await {
        Ok(()) => {
            handoff_confirmed(db, &to).await?;
            tracing::info!(event = "sync_leader_handoff_confirmed");
            Ok(false)
        }
        Err(e) => {
            tracing::info!(event = "sync_leader_handoff_pending", error = %e);
            Ok(true)
        }
    }
}
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
    let sync_on = SYNC_ACTIVE.load(Ordering::SeqCst);
    if sync_on {
        // A change that arrived through sync and took the role from this
        // device becomes a pending hand-off right away: the jobs never stop
        // before the new leader confirms.
        if let Some(me) = my_device_id(db).await {
            if matches!(observe_change(db, &me).await, Ok(Some(_))) {
                return true;
            }
        }
    }
    is_leader_with(db, sync_on).await
}

pub async fn creator(db: &Db) -> Option<String> {
    setting(db, KEY_CREATOR).await.filter(|c| !c.trim().is_empty())
}

/// The relay named `device_id` the group's creator. Written only when it
/// differs, so devices that agree add no ops.
pub async fn set_creator(db: &Db, device_id: &str) -> Result<(), String> {
    let id = device_id.trim();
    if id.is_empty() || creator(db).await.as_deref() == Some(id) {
        return Ok(());
    }
    put_setting(db, KEY_CREATOR, id).await
}

/// This device just created the group: it is the creator and the leader.
pub async fn claim_group(db: &Db, me: &str) -> Result<(), String> {
    put_setting(db, KEY_CREATOR, me).await?;
    set_leader(db, Some(me)).await
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
    /// Created the group: the only device the relay lets remove others.
    pub is_creator: bool,
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
    let creator = creator(db).await;
    let mut out: Vec<DeviceInfo> = rows
        .into_iter()
        .filter_map(|(k, v)| {
            let id = k.strip_prefix(DEVICE_NAME_PREFIX)?.to_string();
            let rec = parse_record(&v);
            Some(DeviceInfo {
                is_self: id == me,
                is_leader: leader.as_deref() == Some(id.as_str()),
                is_creator: creator.as_deref() == Some(id.as_str()),
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
            is_creator: creator.as_deref() == Some(me),
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

/// Forget a device `by` removed: its record, and the leader role passes to
/// `by` if the removed device held it.
pub async fn forget_device(db: &Db, device_id: &str, by: &str) -> Result<(), String> {
    delete_setting(db, &name_key(device_id)).await?;
    if leader(db).await.as_deref() == Some(device_id) {
        set_leader(db, Some(by)).await?;
    }
    Ok(())
}

/// The device a leaving `me` hands its roles to: among the other known
/// devices, the one seen most recently (likeliest to be alive), ties by id.
/// Deterministic, and only the leaving device evaluates it.
pub async fn leave_successor(db: &Db, me: &str) -> Option<String> {
    let devices = list_devices(db, me).await.ok()?;
    devices
        .into_iter()
        .filter(|d| !d.is_self)
        .max_by(|a, b| a.last_seen.cmp(&b.last_seen).then_with(|| b.id.cmp(&a.id)))
        .map(|d| d.id)
}

/// This device is leaving: drop its record and hand its roles on. If it is the
/// creator, the role goes to [`leave_successor`] (returned, for the relay's
/// `sync_revoke{successor}`); if it leads, the leader role goes to the
/// (new) creator, else the successor — cleared only when no device is left.
pub async fn on_leave(db: &Db, me: &str) -> Result<Option<String>, String> {
    let successor = leave_successor(db, me).await;
    let creator = creator(db).await;
    let am_creator = creator.as_deref() == Some(me);
    if am_creator {
        if let Some(next) = &successor {
            put_setting(db, KEY_CREATOR, next).await?;
        }
    }
    delete_setting(db, &name_key(me)).await?;
    if leader(db).await.as_deref() == Some(me) {
        let next = if am_creator { successor.clone() } else { creator.filter(|c| c != me) };
        set_leader(db, next.or_else(|| successor.clone()).as_deref()).await?;
    }
    Ok(successor.filter(|_| am_creator))
}

/// The creator takes the leader role back when the leader's device record is
/// gone (it left or was removed while the hand-over did not reach us).
/// Returns whether it changed anything.
pub async fn heal_leader(db: &Db, me: &str) -> Result<bool, String> {
    let Some(l) = leader(db).await else { return Ok(false) };
    if l == me || creator(db).await.as_deref() != Some(me) || record(db, &l).await.is_some() {
        return Ok(false);
    }
    set_leader(db, Some(me)).await?;
    Ok(true)
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
            if SYNC_ACTIVE.load(Ordering::SeqCst) {
                if let Some(me) = my_device_id(&db).await {
                    if let Err(e) = heal_leader(&db, &me).await {
                        tracing::warn!(event = "sync_leader_heal_failed", error = %e);
                    }
                }
            }
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
    fn old_leader_keeps_jobs_until_the_hand_off_is_confirmed() {
        assert!(keeps_jobs(Some("new"), Some("new")));
        assert!(!keeps_jobs(None, Some("new")), "confirmed");
        assert!(!keeps_jobs(Some("new"), Some("other")), "changed again elsewhere");
        assert!(!keeps_jobs(Some("new"), None));
    }

    /// Review B: the role leaving this device — set here or learned through
    /// sync — is a persisted hand-off; the jobs stay here until the new
    /// leader confirms, and a later change elsewhere ends it.
    #[tokio::test]
    async fn the_device_losing_the_role_keeps_it_until_confirmed() {
        let db = db().await;
        let me = my_device_id(&db).await.unwrap();
        set_leader(&db, Some(&me)).await.unwrap();
        assert_eq!(observe_change(&db, &me).await.unwrap(), None, "first look only records");
        // The change arrives (through sync, or made here).
        set_leader(&db, Some("b")).await.unwrap();
        assert_eq!(observe_change(&db, &me).await.unwrap().as_deref(), Some("b"));
        // Survives a restart: it lives in sync_state.
        assert_eq!(handoff_pending(&db).await.as_deref(), Some("b"));
        assert!(keeps_jobs(handoff_pending(&db).await.as_deref(), leader(&db).await.as_deref()));
        // B unreachable: still pending.
        let still = handoff_step(&db, &me, |_| async { Err("peer_offline".to_string()) }).await.unwrap();
        assert!(still);
        let told = std::sync::Mutex::new(Vec::new());
        let still = handoff_step(&db, &me, |to| {
            told.lock().unwrap().push(to);
            async { Ok(()) }
        })
        .await
        .unwrap();
        assert!(!still);
        assert_eq!(*told.lock().unwrap(), vec!["b".to_string()]);
        assert_eq!(handoff_pending(&db).await, None);

        // Another device was the leader: nothing to hand over from here.
        set_leader(&db, Some("c")).await.unwrap();
        assert_eq!(observe_change(&db, &me).await.unwrap(), None);
        // Pending to b, then the role moves on to c elsewhere: dropped.
        set_leader(&db, Some(&me)).await.unwrap();
        observe_change(&db, &me).await.unwrap();
        set_leader(&db, Some("b")).await.unwrap();
        assert_eq!(observe_change(&db, &me).await.unwrap().as_deref(), Some("b"));
        set_leader(&db, Some("c")).await.unwrap();
        assert_eq!(observe_change(&db, &me).await.unwrap(), None);
        assert_eq!(handoff_pending(&db).await, None);
    }

    /// With no leader every device runs the jobs, so naming one hands them
    /// over from here too.
    #[tokio::test]
    async fn naming_a_first_leader_is_a_hand_off() {
        let db = db().await;
        let me = my_device_id(&db).await.unwrap();
        set_leader(&db, None).await.unwrap();
        assert_eq!(observe_change(&db, &me).await.unwrap(), None);
        set_leader(&db, Some("b")).await.unwrap();
        assert_eq!(observe_change(&db, &me).await.unwrap().as_deref(), Some("b"));
        assert!(keeps_jobs(handoff_pending(&db).await.as_deref(), leader(&db).await.as_deref()));
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

        forget_device(&db, "mini", "me").await.unwrap();
        assert_eq!(leader(&db).await.as_deref(), Some("me"), "removing the leader hands it to the remover");
        assert_eq!(list_devices(&db, "me").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn creator_leads_and_takes_the_role_back() {
        let db = db().await;
        claim_group(&db, "me").await.unwrap();
        assert_eq!(leader(&db).await.as_deref(), Some("me"));
        touch_self(&db, "me", "MacBook").await.unwrap();
        write_record(&db, "mini", &DeviceRecord { name: "Mac mini".into(), last_seen: Some(1) }).await.unwrap();
        let list = list_devices(&db, "me").await.unwrap();
        assert!(list[0].is_creator && !list[1].is_creator);

        // The leader leaves: the role goes back to the creator.
        set_leader(&db, Some("mini")).await.unwrap();
        assert_eq!(on_leave(&db, "mini").await.unwrap(), None, "not the creator: no successor to name");
        assert_eq!(leader(&db).await.as_deref(), Some("me"));
        assert!(record(&db, "mini").await.is_none());

        // The last device leaving clears it.
        assert_eq!(on_leave(&db, "me").await.unwrap(), None);
        assert!(leader(&db).await.is_none());

        // A leader whose record vanished is healed by the creator only.
        claim_group(&db, "me").await.unwrap();
        set_leader(&db, Some("gone")).await.unwrap();
        assert!(!heal_leader(&db, "other").await.unwrap());
        assert!(heal_leader(&db, "me").await.unwrap());
        assert_eq!(leader(&db).await.as_deref(), Some("me"));
        assert!(!heal_leader(&db, "me").await.unwrap());
    }

    #[tokio::test]
    async fn creator_leaving_hands_both_roles_to_one_successor() {
        let db = db().await;
        claim_group(&db, "me").await.unwrap();
        touch_self(&db, "me", "MacBook").await.unwrap();
        for (id, seen) in [("old", 1), ("fresh-b", 9), ("fresh-a", 9)] {
            write_record(&db, id, &DeviceRecord { name: id.into(), last_seen: Some(seen) }).await.unwrap();
        }
        // Most recently seen wins, ties by id: the same answer on every call.
        assert_eq!(leave_successor(&db, "me").await.as_deref(), Some("fresh-a"));
        assert_eq!(on_leave(&db, "me").await.unwrap().as_deref(), Some("fresh-a"));
        assert_eq!(creator(&db).await.as_deref(), Some("fresh-a"));
        assert_eq!(leader(&db).await.as_deref(), Some("fresh-a"), "leader is never cleared while devices remain");

        // A non-creator leader leaving while the creator is unknown: successor.
        sqlx::query("DELETE FROM settings WHERE key = ?").bind(KEY_CREATOR).execute(&db).await.unwrap();
        assert_eq!(on_leave(&db, "fresh-a").await.unwrap(), None);
        assert_eq!(leader(&db).await.as_deref(), Some("fresh-b"));

        // The relay's word on the creator is stored; agreeing writes nothing.
        set_creator(&db, "old").await.unwrap();
        assert_eq!(creator(&db).await.as_deref(), Some("old"));
        let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_outbox").fetch_one(&db).await.unwrap();
        set_creator(&db, "old").await.unwrap();
        let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_outbox").fetch_one(&db).await.unwrap();
        assert_eq!(before, after);
        // The new creator heals a leader whose record is gone.
        set_leader(&db, Some("fresh-a")).await.unwrap();
        assert!(heal_leader(&db, "old").await.unwrap());
        assert_eq!(leader(&db).await.as_deref(), Some("old"));
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
