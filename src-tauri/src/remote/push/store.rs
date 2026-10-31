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
}

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

const COLUMNS: &str = "id, endpoint, p256dh, auth, label, events, created_at, last_ok_at";

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
    }
}

pub async fn list(db: &Db) -> Result<Vec<Subscription>, String> {
    let rows = sqlx::query(&format!("SELECT {COLUMNS} FROM remote_push_subscriptions ORDER BY created_at"))
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
    Db(String),
}

/// Add a subscription, or refresh the keys / label of a known endpoint (a
/// browser re-subscribing). Returns its id.
pub async fn upsert(db: &Db, endpoint: &str, p256dh: &str, auth: &str, label: &str, now: &str) -> Result<String, UpsertError> {
    if let Some(existing) = by_endpoint(db, endpoint).await {
        sqlx::query("UPDATE remote_push_subscriptions SET p256dh = ?, auth = ?, label = ? WHERE id = ?")
            .bind(p256dh)
            .bind(auth)
            .bind(label)
            .bind(&existing.id)
            .execute(db)
            .await
            .map_err(|e| UpsertError::Db(e.to_string()))?;
        return Ok(existing.id);
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM remote_push_subscriptions")
        .fetch_one(db)
        .await
        .map_err(|e| UpsertError::Db(e.to_string()))?;
    if count >= MAX_SUBSCRIPTIONS {
        return Err(UpsertError::TooMany);
    }
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO remote_push_subscriptions (id, endpoint, p256dh, auth, label, events, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(endpoint)
    .bind(p256dh)
    .bind(auth)
    .bind(label)
    .bind(PushEvents::default().to_list())
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
    let _ = sqlx::query("UPDATE remote_push_subscriptions SET last_ok_at = ? WHERE id = ?")
        .bind(now)
        .bind(id)
        .execute(db)
        .await;
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

    #[tokio::test]
    async fn resubscribing_refreshes_instead_of_duplicating() {
        let db = mem_db().await;
        let a = upsert(&db, "https://fcm.googleapis.com/x", "k1", "a1", "Phone", "t0").await.unwrap();
        let b = upsert(&db, "https://fcm.googleapis.com/x", "k2", "a2", "Phone 2", "t1").await.unwrap();
        assert_eq!(a, b);
        let all = list(&db).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!((all[0].p256dh.as_str(), all[0].label.as_str()), ("k2", "Phone 2"));
        assert_eq!(all[0].created_at, "t0");
        assert_eq!(all[0].events, PushEvents::default());
    }

    #[tokio::test]
    async fn the_device_count_is_capped() {
        let db = mem_db().await;
        for i in 0..MAX_SUBSCRIPTIONS {
            upsert(&db, &format!("https://fcm.googleapis.com/{i}"), "k", "a", "", "t").await.unwrap();
        }
        assert_eq!(
            upsert(&db, "https://fcm.googleapis.com/new", "k", "a", "", "t").await,
            Err(UpsertError::TooMany)
        );
        // A known endpoint can still refresh.
        assert!(upsert(&db, "https://fcm.googleapis.com/0", "k", "a", "", "t").await.is_ok());
    }

    #[tokio::test]
    async fn event_switches_default_on_and_persist() {
        let db = mem_db().await;
        assert_eq!(events(&db).await, PushEvents::default());
        let off = PushEvents { pending: true, done: false, failed: true };
        set_events(&db, off).await.unwrap();
        assert_eq!(events(&db).await, off);
    }
}
