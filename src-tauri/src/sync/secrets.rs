//! Keychain sync (design D4, §9b): `secret` ops carry one Keychain item each,
//! `pk = {"service", "account"}`, `row = {"value"}`, sealed with the group key
//! like every op. Only while `sync_secrets_enabled` (machine-local setting,
//! default on).
//!
//! Changes are found by hashing every item on the full-scan cadence and
//! comparing with the hashes stored in `sync_state` (`secret_hash:<pk>`,
//! salted per device); values are never written to the DB. The sync group's
//! own secrets and Remote Control's per-machine credentials are never listed
//! (see `crate::secrets::sync_items`), and never accepted from a peer.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use sqlx::SqliteConnection;

use super::clock::{wall_ms, Hlc};
use super::op::{canonical_json, Op, OpKind};
use crate::db::Db;

pub const TBL: &str = "secret";
pub const SETTING: &str = "sync_secrets_enabled";
const HASH_PREFIX: &str = "secret_hash:";
const SALT_KEY: &str = "secret_salt";

/// A Keychain item as sync names it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SecretId {
    pub service: String,
    pub account: String,
}

impl SecretId {
    pub fn new(service: &str, account: &str) -> Self {
        SecretId { service: service.into(), account: account.into() }
    }

    pub fn pk(&self) -> Value {
        json!({ "service": self.service, "account": self.account })
    }

    pub fn from_pk(pk: &Value) -> Option<Self> {
        Some(SecretId::new(pk.get("service")?.as_str()?, pk.get("account")?.as_str()?))
    }
}

/// The Keychain as sync sees it. Tests use an in-memory fake.
pub trait SecretBackend: Send + Sync {
    /// Every syncable item. Must fail (not return empty) when the Keychain
    /// cannot be read, or the scan would push deletes for everything.
    fn list(&self) -> Result<BTreeMap<SecretId, String>, String>;
    /// Store (`Some`) or remove (`None`) one item.
    fn set(&self, id: &SecretId, value: Option<&str>) -> Result<(), String>;
}

/// The real Keychain (Devdy's consolidated item, `crate::secrets`).
pub struct KeychainBackend;

impl SecretBackend for KeychainBackend {
    fn list(&self) -> Result<BTreeMap<SecretId, String>, String> {
        Ok(crate::secrets::sync_items()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|(service, account, value)| (SecretId { service, account }, value))
            .collect())
    }

    fn set(&self, id: &SecretId, value: Option<&str>) -> Result<(), String> {
        crate::secrets::sync_set_item(&id.service, &id.account, value).map_err(|e| e.to_string())
    }
}

pub type Backend = Arc<dyn SecretBackend>;

fn err(e: sqlx::Error) -> String {
    e.to_string()
}

/// `sync_secrets_enabled` (machine-local, default on).
pub async fn enabled(conn: &mut SqliteConnection) -> Result<bool, String> {
    let v: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(SETTING)
        .fetch_optional(&mut *conn)
        .await
        .map_err(err)?;
    Ok(v.as_deref() != Some("false"))
}

async fn salt(conn: &mut SqliteConnection) -> Result<String, String> {
    let v: Option<String> = sqlx::query_scalar("SELECT value FROM sync_state WHERE key = ?")
        .bind(SALT_KEY)
        .fetch_optional(&mut *conn)
        .await
        .map_err(err)?;
    if let Some(v) = v {
        return Ok(v);
    }
    let v = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    sqlx::query("INSERT INTO sync_state (key, value) VALUES (?, ?)")
        .bind(SALT_KEY)
        .bind(&v)
        .execute(&mut *conn)
        .await
        .map_err(err)?;
    Ok(v)
}

fn hash(salt: &str, id: &SecretId, value: &str) -> String {
    let mut h = Sha256::new();
    for part in [salt, &id.service, &id.account, value] {
        h.update(part.as_bytes());
        h.update([0u8]);
    }
    hex::encode(h.finalize())
}

fn hash_key(id: &SecretId) -> String {
    format!("{HASH_PREFIX}{}", canonical_json(&id.pk()))
}

async fn set_hash(conn: &mut SqliteConnection, id: &SecretId, h: Option<&str>) -> Result<(), String> {
    match h {
        Some(h) => sqlx::query(
            "INSERT INTO sync_state (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(hash_key(id))
        .bind(h),
        None => sqlx::query("DELETE FROM sync_state WHERE key = ?").bind(hash_key(id)),
    }
    .execute(&mut *conn)
    .await
    .map_err(err)?;
    Ok(())
}

/// Hash every item and queue `secret` ops for the ones that changed since the
/// last scan. Returns how many were queued.
pub async fn scan(db: &Db, backend: &dyn SecretBackend) -> Result<usize, String> {
    let mut tx = db.begin().await.map_err(err)?;
    if !enabled(&mut tx).await? {
        return Ok(0);
    }
    let items = backend.list()?;
    let salt = salt(&mut tx).await?;
    let stored: BTreeMap<String, String> = sqlx::query_as::<_, (String, String)>(
        "SELECT key, value FROM sync_state WHERE key LIKE 'secret\\_hash:%' ESCAPE '\\'",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(err)?
    .into_iter()
    .collect();
    let mut queued = 0;
    let mut seen = std::collections::BTreeSet::new();
    for (id, value) in &items {
        let key = hash_key(id);
        let h = hash(&salt, id, value);
        seen.insert(key.clone());
        if stored.get(&key) == Some(&h) {
            continue;
        }
        set_hash(&mut tx, id, Some(&h)).await?;
        queue(&mut tx, id, "U").await?;
        queued += 1;
    }
    // Gone from the Keychain. An empty listing next to known items looks like
    // a read problem rather than "the user removed everything": no deletes.
    if !items.is_empty() {
        for key in stored.keys().filter(|k| !seen.contains(*k)) {
            let Some(id) = key.strip_prefix(HASH_PREFIX).and_then(|pk| serde_json::from_str(pk).ok()).and_then(|pk: Value| SecretId::from_pk(&pk)) else {
                continue;
            };
            set_hash(&mut tx, &id, None).await?;
            queue(&mut tx, &id, "D").await?;
            queued += 1;
        }
    }
    tx.commit().await.map_err(err)?;
    Ok(queued)
}

async fn queue(conn: &mut SqliteConnection, id: &SecretId, op: &str) -> Result<(), String> {
    sqlx::query("INSERT INTO sync_outbox (tbl, pk, op, changed_at) VALUES (?, ?, ?, ?)")
        .bind(TBL)
        .bind(canonical_json(&id.pk()))
        .bind(op)
        .bind(wall_ms() as i64)
        .execute(&mut *conn)
        .await
        .map_err(err)?;
    Ok(())
}

/// The op row for an outbox entry from the current Keychain content. `None`
/// drops the entry (secrets sync off, or not a syncable item).
pub fn pending_row(
    items: &BTreeMap<SecretId, String>,
    pk: &Value,
) -> Option<(OpKind, Option<Map<String, Value>>)> {
    let id = SecretId::from_pk(pk).filter(|id| crate::secrets::is_sync_item(&id.service, &id.account))?;
    Some(match items.get(&id) {
        Some(v) => (OpKind::Upsert, json!({ "value": v }).as_object().cloned()),
        None => (OpKind::Delete, None),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Applied,
    Skipped,
}

/// Apply one remote `secret` op (LWW by HLC, like a row).
pub async fn apply_op(db: &Db, backend: &dyn SecretBackend, op: &Op) -> Result<Outcome, String> {
    let id = SecretId::from_pk(&op.pk).ok_or_else(|| format!("malformed secret pk {}", op.pk))?;
    let mut tx = db.begin().await.map_err(err)?;
    if !enabled(&mut tx).await? || !crate::secrets::is_sync_item(&id.service, &id.account) {
        return Ok(Outcome::Skipped);
    }
    let key = op.pk_key();
    let known: Option<String> = sqlx::query_scalar("SELECT hlc FROM sync_rows WHERE tbl = ? AND pk = ?")
        .bind(TBL)
        .bind(&key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(err)?;
    if known.and_then(|s| s.parse::<Hlc>().ok()).is_some_and(|h| h >= op.hlc) {
        return Ok(Outcome::Skipped);
    }
    let local_changed: Option<i64> =
        sqlx::query_scalar("SELECT MAX(changed_at) FROM sync_outbox WHERE tbl = ? AND pk = ?")
            .bind(TBL)
            .bind(&key)
            .fetch_one(&mut *tx)
            .await
            .map_err(err)?;
    if local_changed.is_some_and(|c| c.max(0) as u64 >= op.hlc.millis) {
        return Ok(Outcome::Skipped);
    }
    let value = match op.kind {
        OpKind::Upsert => Some(
            op.row
                .as_ref()
                .and_then(|r| r.get("value"))
                .and_then(Value::as_str)
                .ok_or("secret op without value")?
                .to_string(),
        ),
        OpKind::Delete => None,
    };
    backend.set(&id, value.as_deref())?;
    // Remember the applied value's hash so the next scan does not echo it.
    let salt = salt(&mut tx).await?;
    set_hash(&mut tx, &id, value.as_deref().map(|v| hash(&salt, &id, v)).as_deref()).await?;
    sqlx::query("DELETE FROM sync_outbox WHERE tbl = ? AND pk = ?")
        .bind(TBL)
        .bind(&key)
        .execute(&mut *tx)
        .await
        .map_err(err)?;
    sqlx::query(
        "INSERT INTO sync_rows (tbl, pk, hlc, deleted) VALUES (?, ?, ?, ?) \
         ON CONFLICT(tbl, pk) DO UPDATE SET hlc = excluded.hlc, deleted = excluded.deleted",
    )
    .bind(TBL)
    .bind(&key)
    .bind(op.hlc.to_string())
    .bind((op.kind == OpKind::Delete) as i64)
    .execute(&mut *tx)
    .await
    .map_err(err)?;
    tx.commit().await.map_err(err)?;
    Ok(Outcome::Applied)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::sync::files::tests::mem_db;
    use crate::sync::op::OP_VERSION;
    use crate::sync::SCHEMA_VERSION;
    use std::sync::Mutex;

    /// In-memory Keychain.
    #[derive(Default)]
    pub struct FakeKeychain {
        pub items: Mutex<BTreeMap<SecretId, String>>,
        pub broken: Mutex<bool>,
    }

    impl FakeKeychain {
        pub fn put(&self, service: &str, account: &str, value: &str) {
            self.items.lock().unwrap().insert(SecretId::new(service, account), value.into());
        }
        pub fn get(&self, service: &str, account: &str) -> Option<String> {
            self.items.lock().unwrap().get(&SecretId::new(service, account)).cloned()
        }
    }

    impl SecretBackend for FakeKeychain {
        fn list(&self) -> Result<BTreeMap<SecretId, String>, String> {
            if *self.broken.lock().unwrap() {
                return Err("keychain locked".into());
            }
            Ok(self.items.lock().unwrap().clone())
        }
        fn set(&self, id: &SecretId, value: Option<&str>) -> Result<(), String> {
            let mut items = self.items.lock().unwrap();
            match value {
                Some(v) => items.insert(id.clone(), v.into()),
                None => items.remove(id),
            };
            Ok(())
        }
    }

    async fn outbox(db: &Db) -> Vec<(String, String)> {
        sqlx::query_as("SELECT pk, op FROM sync_outbox WHERE tbl = 'secret' ORDER BY seq").fetch_all(db).await.unwrap()
    }

    fn op(id: &SecretId, value: Option<&str>, millis: u64) -> Op {
        Op {
            v: OP_VERSION,
            schema: SCHEMA_VERSION,
            hlc: Hlc::new(millis, 0, "dev-a"),
            tbl: TBL.into(),
            pk: id.pk(),
            kind: if value.is_some() { OpKind::Upsert } else { OpKind::Delete },
            row: value.map(|v| json!({ "value": v }).as_object().unwrap().clone()),
        }
    }

    #[tokio::test]
    async fn scan_hashes_and_queues_changes_only() {
        let db = mem_db().await;
        let kc = FakeKeychain::default();
        kc.put("github", "acc1", "ghp_secret");
        kc.put("mcp", "srv", r#"{"env":{"K":"v"},"headers":{}}"#);
        assert_eq!(scan(&db, &kc).await.unwrap(), 2);
        assert_eq!(scan(&db, &kc).await.unwrap(), 0, "unchanged");
        // Only hashes are stored, never values.
        let state: Vec<String> = sqlx::query_scalar("SELECT value FROM sync_state").fetch_all(&db).await.unwrap();
        assert!(state.iter().all(|v| !v.contains("ghp_secret") && !v.contains("\"K\"")));

        kc.put("github", "acc1", "ghp_rotated");
        kc.items.lock().unwrap().remove(&SecretId::new("mcp", "srv"));
        assert_eq!(scan(&db, &kc).await.unwrap(), 2);
        let ob = outbox(&db).await;
        assert_eq!(&ob[2..], &[
            (r#"{"account":"acc1","service":"github"}"#.to_string(), "U".to_string()),
            (r#"{"account":"srv","service":"mcp"}"#.to_string(), "D".to_string()),
        ]);

        // pending rows come from the live Keychain.
        let items = kc.list().unwrap();
        let (kind, row) = pending_row(&items, &SecretId::new("github", "acc1").pk()).unwrap();
        assert_eq!((kind, row.unwrap()["value"].clone()), (OpKind::Upsert, json!("ghp_rotated")));
        assert_eq!(pending_row(&items, &SecretId::new("mcp", "srv").pk()).unwrap().0, OpKind::Delete);
        assert!(pending_row(&items, &SecretId::new("sync", "group").pk()).is_none());

        // A locked Keychain is an error; an empty listing never mass-deletes.
        *kc.broken.lock().unwrap() = true;
        assert!(scan(&db, &kc).await.is_err());
        *kc.broken.lock().unwrap() = false;
        kc.items.lock().unwrap().clear();
        assert_eq!(scan(&db, &kc).await.unwrap(), 0);

        // Disabled: nothing is scanned.
        sqlx::query("INSERT INTO settings (key, value) VALUES (?, 'false')").bind(SETTING).execute(&db).await.unwrap();
        kc.put("gitlab", "g", "glpat");
        assert_eq!(scan(&db, &kc).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn apply_sets_keychain_without_echo() {
        let db = mem_db().await;
        let kc = FakeKeychain::default();
        let id = SecretId::new("github", "acc1");
        assert_eq!(apply_op(&db, &kc, &op(&id, Some("ghp_remote"), 1_000)).await.unwrap(), Outcome::Applied);
        assert_eq!(kc.get("github", "acc1").as_deref(), Some("ghp_remote"));
        assert_eq!(scan(&db, &kc).await.unwrap(), 0, "the applied value is not pushed back");
        // Older op loses.
        assert_eq!(apply_op(&db, &kc, &op(&id, Some("old"), 500)).await.unwrap(), Outcome::Skipped);
        // A newer unpushed local change wins over an older remote op.
        kc.put("github", "acc1", "ghp_local");
        assert_eq!(scan(&db, &kc).await.unwrap(), 1);
        assert_eq!(apply_op(&db, &kc, &op(&id, Some("stale"), 2_000)).await.unwrap(), Outcome::Skipped);
        assert_eq!(kc.get("github", "acc1").as_deref(), Some("ghp_local"));
        // Delete.
        let far = wall_ms() + 60_000;
        assert_eq!(apply_op(&db, &kc, &op(&id, None, far)).await.unwrap(), Outcome::Applied);
        assert_eq!(kc.get("github", "acc1"), None);
        assert!(outbox(&db).await.is_empty());
        // Per-machine items are never accepted.
        let own = SecretId::new("sync", "group");
        assert_eq!(apply_op(&db, &kc, &op(&own, Some("x"), far + 1)).await.unwrap(), Outcome::Skipped);
        assert_eq!(kc.get("sync", "group"), None);
        // Disabled: skipped.
        sqlx::query("INSERT INTO settings (key, value) VALUES (?, 'false')").bind(SETTING).execute(&db).await.unwrap();
        assert_eq!(apply_op(&db, &kc, &op(&id, Some("v"), far + 2)).await.unwrap(), Outcome::Skipped);
        assert_eq!(kc.get("github", "acc1"), None);
    }
}
