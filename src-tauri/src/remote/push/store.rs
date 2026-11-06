//! Push subscriptions (`remote_push_subscriptions`) and the push settings.

use super::coalesce::PushEvents;
use crate::db::Db;
use serde::Serialize;
use sqlx::Row;

/// Settings key: the desktop's per-kind switches (JSON [`PushEvents`]).
pub const KEY_PUSH_EVENTS: &str = "remote_push_events";
/// Settings key: optional VAPID contact (`mailto:` / `https:`); defaults to the
/// relay's origin.
pub const KEY_PUSH_CONTACT: &str = "remote_push_contact";

/// Cap on stored devices: each one costs an HTTPS request per notification.
pub const MAX_SUBSCRIPTIONS: i64 = 20;

/// One subscribed browser.
#[derive(Debug, Clone)]
pub struct Subscription {
    pub id: String,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub label: String,
    pub events: PushEvents,
    pub created_at: String,
    pub last_ok_at: Option<String>,
    /// The Host public key it subscribed with (base64url).
    pub vapid_key: String,
    /// Read by the store's own queries (and tests), not through this struct.
    #[cfg_attr(not(test), allow(dead_code))]
    pub session_tag: String,
    pub removed: bool,
    #[cfg_attr(not(test), allow(dead_code))]
    pub auth_failures: i64,
}

/// Consecutive 401/403 answers after which a subscription counts as gone.
pub const MAX_AUTH_FAILURES: i64 = 3;

/// What the desktop list shows: never the endpoint or keys.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    pub id: String,
    pub label: String,
    pub created_at: String,
    pub last_ok_at: Option<String>,
}

impl From<&Subscription> for DeviceInfo {
    fn from(s: &Subscription) -> Self {
        DeviceInfo {
            id: s.id.clone(),
            label: s.label.clone(),
            created_at: s.created_at.clone(),
            last_ok_at: s.last_ok_at.clone(),
        }
    }
}

const COLUMNS: &str =
    "id, endpoint, p256dh, auth, label, events, created_at, last_ok_at, vapid_key, session_tag, removed_at, auth_failures";

fn from_row(r: &sqlx::sqlite::SqliteRow) -> Subscription {
    Subscription {
        id: r.get("id"),
        endpoint: r.get("endpoint"),
        p256dh: r.get("p256dh"),
        auth: r.get("auth"),
        label: r.get("label"),
        events: PushEvents::from_list(&r.get::<String, _>("events")),
        created_at: r.get("created_at"),
        last_ok_at: r.get("last_ok_at"),
        vapid_key: r.get("vapid_key"),
        session_tag: r.get("session_tag"),
        removed: r.get::<Option<String>, _>("removed_at").is_some(),
        auth_failures: r.get("auth_failures"),
    }
}

/// Active subscriptions (not removed on the desktop).
pub async fn list(db: &Db) -> Result<Vec<Subscription>, String> {
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM remote_push_subscriptions WHERE removed_at IS NULL ORDER BY created_at"
    ))
        .fetch_all(db)
        .await
        .map_err(|e| e.to_string())?;
    Ok(rows.iter().map(from_row).collect())
}

pub async fn by_endpoint(db: &Db, endpoint: &str) -> Option<Subscription> {
    sqlx::query(&format!("SELECT {COLUMNS} FROM remote_push_subscriptions WHERE endpoint = ?"))
        .bind(endpoint)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .map(|r| from_row(&r))
}

pub async fn by_id(db: &Db, id: &str) -> Option<Subscription> {
    sqlx::query(&format!("SELECT {COLUMNS} FROM remote_push_subscriptions WHERE id = ?"))
        .bind(id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .map(|r| from_row(&r))
}

/// Why a subscribe was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum UpsertError {
    TooMany,
    /// An automatic re-subscribe of a device removed on the desktop.
    Removed,
    Db(String),
}

/// A subscribe as the Host stores it.
#[derive(Debug, Clone, Copy)]
pub struct NewRow<'a> {
    pub endpoint: &'a str,
    pub p256dh: &'a str,
    pub auth: &'a str,
    pub label: &'a str,
    pub vapid_key: &'a str,
    pub session_tag: &'a str,
    /// The phone re-subscribed on its own (not the user tapping "turn on"):
    /// a desktop removal stands.
    pub automatic: bool,
}

/// Add a subscription, or refresh a known endpoint (a browser re-subscribing).
/// Returns its id.
pub async fn upsert(db: &Db, row: NewRow<'_>, now: &str) -> Result<String, UpsertError> {
    if let Some(existing) = by_endpoint(db, row.endpoint).await {
        if existing.removed && row.automatic {
            return Err(UpsertError::Removed);
        }
        sqlx::query(
            "UPDATE remote_push_subscriptions SET p256dh = ?, auth = ?, label = ?, vapid_key = ?, \
             session_tag = ?, removed_at = NULL, auth_failures = 0 WHERE id = ?",
        )
        .bind(row.p256dh)
        .bind(row.auth)
        .bind(row.label)
        .bind(row.vapid_key)
        .bind(row.session_tag)
        .bind(&existing.id)
        .execute(db)
        .await
        .map_err(|e| UpsertError::Db(e.to_string()))?;
        return Ok(existing.id);
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM remote_push_subscriptions WHERE removed_at IS NULL")
        .fetch_one(db)
        .await
        .map_err(|e| UpsertError::Db(e.to_string()))?;
    if count >= MAX_SUBSCRIPTIONS {
        return Err(UpsertError::TooMany);
    }
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO remote_push_subscriptions \
         (id, endpoint, p256dh, auth, label, events, vapid_key, session_tag, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(row.endpoint)
    .bind(row.p256dh)
    .bind(row.auth)
    .bind(row.label)
    .bind(PushEvents::default().to_list())
    .bind(row.vapid_key)
    .bind(row.session_tag)
    .bind(now)
    .execute(db)
    .await
    .map_err(|e| UpsertError::Db(e.to_string()))?;
    Ok(id)
}

/// Remove by endpoint; whether a row went.
pub async fn remove_endpoint(db: &Db, endpoint: &str) -> bool {
    sqlx::query("DELETE FROM remote_push_subscriptions WHERE endpoint = ?")
        .bind(endpoint)
        .execute(db)
        .await
        .map(|r| r.rows_affected() > 0)
        .unwrap_or(false)
}

/// Removed on the desktop: stop sending, keep a tombstone (see the migration).
pub async fn mark_removed(db: &Db, id: &str, now: &str) -> bool {
    sqlx::query("UPDATE remote_push_subscriptions SET removed_at = ? WHERE id = ? AND removed_at IS NULL")
        .bind(now)
        .bind(id)
        .execute(db)
        .await
        .map(|r| r.rows_affected() > 0)
        .unwrap_or(false)
}

/// Drop every device a remote session subscribed (it was ended or kicked).
pub async fn remove_session(db: &Db, session_tag: &str) -> u64 {
    if session_tag.is_empty() {
        return 0;
    }
    sqlx::query("DELETE FROM remote_push_subscriptions WHERE session_tag = ?")
        .bind(session_tag)
        .execute(db)
        .await
        .map(|r| r.rows_affected())
        .unwrap_or(0)
}

/// Drop rows subscribed with another Host key: no push to them can verify.
pub async fn remove_other_keys(db: &Db, vapid_key: &str) -> u64 {
    sqlx::query("DELETE FROM remote_push_subscriptions WHERE vapid_key <> ?")
        .bind(vapid_key)
        .execute(db)
        .await
        .map(|r| r.rows_affected())
        .unwrap_or(0)
}

/// Count one more 401/403; true when that makes the subscription gone.
pub async fn note_auth_failure(db: &Db, id: &str) -> bool {
    let n: Option<i64> = sqlx::query_scalar(
        "UPDATE remote_push_subscriptions SET auth_failures = auth_failures + 1 WHERE id = ? RETURNING auth_failures",
    )
    .bind(id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    n.is_some_and(|n| n >= MAX_AUTH_FAILURES)
}

/// Remove by id; whether a row went.
pub async fn remove_id(db: &Db, id: &str) -> bool {
    sqlx::query("DELETE FROM remote_push_subscriptions WHERE id = ?")
        .bind(id)
        .execute(db)
        .await
        .map(|r| r.rows_affected() > 0)
        .unwrap_or(false)
}

pub async fn mark_ok(db: &Db, id: &str, now: &str) {
    let _ = sqlx::query("UPDATE remote_push_subscriptions SET last_ok_at = ?, auth_failures = 0 WHERE id = ?")
        .bind(now)
        .bind(id)
        .execute(db)
        .await;
}

/// Save one device's own per-kind switches (the phone's Setting → Notifications).
/// `false` when no live subscription has that endpoint.
pub async fn set_device_events(db: &Db, endpoint: &str, events: PushEvents) -> Result<bool, String> {
    sqlx::query("UPDATE remote_push_subscriptions SET events = ? WHERE endpoint = ? AND removed_at IS NULL")
        .bind(events.to_list())
        .bind(endpoint)
        .execute(db)
        .await
        .map(|r| r.rows_affected() > 0)
        .map_err(|e| e.to_string())
}

/// The desktop's per-kind switches (all on until changed).
pub async fn events(db: &Db) -> PushEvents {
    crate::remote::get_setting(db, KEY_PUSH_EVENTS)
        .await
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default()
}

pub async fn set_events(db: &Db, events: PushEvents) -> Result<(), String> {
    let json = serde_json::to_string(&events).map_err(|e| e.to_string())?;
    crate::remote::set_setting(db, KEY_PUSH_EVENTS, &json).await
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) async fn mem_db() -> Db {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&db).await.unwrap();
        db
    }

    pub(crate) fn row(endpoint: &str) -> NewRow<'_> {
        NewRow {
            endpoint,
            p256dh: "k",
            auth: "a",
            label: "",
            vapid_key: "V1",
            session_tag: "S1",
            automatic: false,
        }
    }

    #[tokio::test]
    async fn resubscribing_refreshes_instead_of_duplicating() {
        let db = mem_db().await;
        let e = "https://fcm.googleapis.com/x";
        let a = upsert(&db, NewRow { p256dh: "k1", label: "Phone", ..row(e) }, "t0").await.unwrap();
        let b = upsert(&db, NewRow { p256dh: "k2", label: "Phone 2", ..row(e) }, "t1").await.unwrap();
        assert_eq!(a, b);
        let all = list(&db).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!((all[0].p256dh.as_str(), all[0].label.as_str()), ("k2", "Phone 2"));
        assert_eq!(all[0].created_at, "t0");
        assert_eq!(all[0].events, PushEvents::default());
    }

    #[tokio::test]
    async fn a_device_keeps_its_own_kinds_across_resubscribes() {
        let db = mem_db().await;
        let e = "https://fcm.googleapis.com/x";
        upsert(&db, row(e), "t0").await.unwrap();
        let mine = PushEvents { done: false, focus: false, ..Default::default() };
        assert_eq!(set_device_events(&db, e, mine).await, Ok(true));
        upsert(&db, NewRow { automatic: true, ..row(e) }, "t1").await.unwrap();
        assert_eq!(by_endpoint(&db, e).await.unwrap().events, mine);
        // The desktop's own switches are a separate setting.
        assert_eq!(events(&db).await, PushEvents::default());
        // Unknown or desktop-removed devices are not written.
        assert_eq!(set_device_events(&db, "https://fcm.googleapis.com/none", mine).await, Ok(false));
        remove_endpoint(&db, e).await;
        assert_eq!(set_device_events(&db, e, mine).await, Ok(false));
    }

    #[tokio::test]
    async fn the_device_count_is_capped() {
        let db = mem_db().await;
        for i in 0..MAX_SUBSCRIPTIONS {
            upsert(&db, row(&format!("https://fcm.googleapis.com/{i}")), "t").await.unwrap();
        }
        assert_eq!(upsert(&db, row("https://fcm.googleapis.com/new"), "t").await, Err(UpsertError::TooMany));
        // A known endpoint can still refresh.
        assert!(upsert(&db, row("https://fcm.googleapis.com/0"), "t").await.is_ok());
    }

    #[tokio::test]
    async fn a_desktop_removal_survives_automatic_resubscribes_only() {
        let db = mem_db().await;
        let e = "https://fcm.googleapis.com/x";
        let id = upsert(&db, row(e), "t").await.unwrap();
        assert!(mark_removed(&db, &id, "t1").await);
        assert!(list(&db).await.unwrap().is_empty(), "no longer sent to");
        assert_eq!(upsert(&db, NewRow { automatic: true, ..row(e) }, "t2").await, Err(UpsertError::Removed));
        // The user turning it on again on the phone wins.
        assert_eq!(upsert(&db, row(e), "t3").await.unwrap(), id);
        assert_eq!(list(&db).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn ending_a_session_drops_its_devices() {
        let db = mem_db().await;
        upsert(&db, row("https://fcm.googleapis.com/a"), "t").await.unwrap();
        upsert(&db, NewRow { session_tag: "S2", ..row("https://fcm.googleapis.com/b") }, "t").await.unwrap();
        assert_eq!(remove_session(&db, "S1").await, 1);
        assert_eq!(remove_session(&db, "").await, 0, "an empty tag never matches");
        let left = list(&db).await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].session_tag, "S2");
    }

    #[tokio::test]
    async fn rows_for_another_host_key_are_dropped() {
        let db = mem_db().await;
        upsert(&db, row("https://fcm.googleapis.com/a"), "t").await.unwrap();
        upsert(&db, NewRow { vapid_key: "OLD", ..row("https://fcm.googleapis.com/b") }, "t").await.unwrap();
        assert_eq!(remove_other_keys(&db, "V1").await, 1);
        assert_eq!(list(&db).await.unwrap()[0].vapid_key, "V1");
    }

    #[tokio::test]
    async fn event_switches_default_on_and_persist() {
        let db = mem_db().await;
        assert_eq!(events(&db).await, PushEvents::default());
        let off = PushEvents { pending: true, done: false, failed: true, focus: false };
        set_events(&db, off).await.unwrap();
        assert_eq!(events(&db).await, off);
        // Saved before Focus reminders existed: they come on.
        crate::remote::set_setting(&db, KEY_PUSH_EVENTS, r#"{"pending":false,"done":true,"failed":true}"#)
            .await
            .unwrap();
        assert_eq!(events(&db).await, PushEvents { pending: false, done: true, failed: true, focus: true });
    }
}
