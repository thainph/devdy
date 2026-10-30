//! Change capture: AFTER INSERT/UPDATE/DELETE triggers on every synced table,
//! generated from the registry and written into `sync_outbox`. Triggers (not
//! Rust hooks) are required because sidecar-mcp writes the same DB directly via
//! `node:sqlite`, so the SQL may only use core SQLite functions.
//!
//! The triggers stay silent while `sync_apply_guard.active = 1` (remote ops
//! being applied), so applied ops are never echoed back, and while this
//! device is in no sync group (`sync_state.member` != '1'), so the outbox does
//! not grow forever: creating a group queues every row (`enqueue_all`) and
//! joining one queues the local-only rows (bootstrap merge).

use sha2::{Digest, Sha256};
use sqlx::Row;

use super::schema::{lit, pk_sql, q, TableSpec, SYNCED_TABLES};
use crate::db::Db;

/// Every generated trigger name starts with this, so stale ones (a table that
/// left the registry) can be found and dropped.
const PREFIX: &str = "sync_cap_";
const HASH_KEY: &str = "capture_hash";
/// `sync_state` key: '1' while this device is in a sync group (set on create /
/// join before the rows are queued, cleared on leave).
pub const MEMBER_KEY: &str = "member";

/// Wall time in ms, computed in SQL.
const NOW_MS: &str = "CAST((julianday('now') - 2440587.5) * 86400000.0 AS INTEGER)";
const GUARD_OFF: &str = "COALESCE((SELECT active FROM sync_apply_guard WHERE id = 1), 0) = 0 \
    AND EXISTS (SELECT 1 FROM sync_state WHERE key = 'member' AND value = '1')";

fn filter_sql(spec: &TableSpec, row: &str) -> Option<String> {
    let f = spec.filter?;
    let col = format!("{row}.{}", q(f.col));
    let values: Vec<String> = f.allowed.iter().map(|v| lit(v)).collect();
    let mut any = vec![format!("{col} IN ({})", values.join(", "))];
    // substr(), not LIKE: keys may contain `_` / `%`.
    any.extend(f.prefixes.iter().map(|p| format!("substr({col}, 1, {}) = {}", p.len(), lit(p))));
    Some(format!("({})", any.join(" OR ")))
}

fn outbox_insert(spec: &TableSpec, row: &str, op: &str) -> String {
    format!(
        "INSERT INTO sync_outbox (tbl, pk, op, changed_at) VALUES ({}, {}, '{op}', {NOW_MS});",
        lit(spec.name),
        pk_sql(spec, row)
    )
}

/// The `CREATE TRIGGER` statements for one table.
pub fn trigger_sql(spec: &TableSpec) -> Vec<String> {
    let t = q(spec.name);
    let when = |rows: &[&str]| {
        let filters: Vec<String> = rows.iter().filter_map(|r| filter_sql(spec, r)).collect();
        if filters.is_empty() {
            GUARD_OFF.to_string()
        } else {
            format!("{GUARD_OFF} AND ({})", filters.join(" OR "))
        }
    };
    let ins = format!(
        "CREATE TRIGGER {} AFTER INSERT ON {t} WHEN {} BEGIN {} END",
        q(&format!("{PREFIX}{}_ins", spec.name)),
        when(&["NEW"]),
        outbox_insert(spec, "NEW", "I"),
    );
    // A pk change is a delete of the old key plus an upsert of the new one.
    let old_key_gone = format!(
        "INSERT INTO sync_outbox (tbl, pk, op, changed_at) SELECT {}, {}, 'D', {NOW_MS} WHERE {} IS NOT {};",
        lit(spec.name),
        pk_sql(spec, "OLD"),
        pk_sql(spec, "OLD"),
        pk_sql(spec, "NEW"),
    );
    let upd = format!(
        "CREATE TRIGGER {} AFTER UPDATE ON {t} WHEN {} BEGIN {} {old_key_gone} END",
        q(&format!("{PREFIX}{}_upd", spec.name)),
        when(&["NEW", "OLD"]),
        outbox_insert(spec, "NEW", "U"),
    );
    let del = format!(
        "CREATE TRIGGER {} AFTER DELETE ON {t} WHEN {} BEGIN {} END",
        q(&format!("{PREFIX}{}_del", spec.name)),
        when(&["OLD"]),
        outbox_insert(spec, "OLD", "D"),
    );
    vec![ins, upd, del]
}

fn all_trigger_sql() -> Vec<String> {
    SYNCED_TABLES.iter().flat_map(trigger_sql).collect()
}

/// Version of the generated triggers: changes whenever the registry does.
pub fn spec_hash() -> String {
    let mut h = Sha256::new();
    for s in all_trigger_sql() {
        h.update(s.as_bytes());
        h.update([0u8]);
    }
    hex::encode(h.finalize())
}

/// Mark this device in / out of a sync group. Leaving drops the outbox: what
/// was not pushed belongs to no group any more (a later join queues the
/// local-only rows again).
// Wired by pair/bootstrap (create / join / leave).
#[cfg_attr(not(test), allow(dead_code))]
pub async fn set_member(db: &Db, member: bool) -> Result<(), String> {
    let err = |e: sqlx::Error| e.to_string();
    let mut tx = db.begin().await.map_err(err)?;
    sqlx::query("INSERT INTO sync_state (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
        .bind(MEMBER_KEY)
        .bind(if member { "1" } else { "0" })
        .execute(&mut *tx)
        .await
        .map_err(err)?;
    if !member {
        sqlx::query("DELETE FROM sync_outbox").execute(&mut *tx).await.map_err(err)?;
    }
    tx.commit().await.map_err(err)
}

/// Startup: a device that synced before the membership flag existed (it has
/// a pull cursor or pushed / applied rows) is a member; any other device
/// without the flag drops what the triggers queued before they were gated.
pub async fn settle_membership(db: &Db) -> Result<(), String> {
    let err = |e: sqlx::Error| e.to_string();
    let mut tx = db.begin().await.map_err(err)?;
    sqlx::query(
        "INSERT INTO sync_state (key, value) SELECT ?, '1' \
         WHERE NOT EXISTS (SELECT 1 FROM sync_state WHERE key = ?) \
           AND (EXISTS (SELECT 1 FROM sync_state WHERE key = 'pull_cursor') OR EXISTS (SELECT 1 FROM sync_rows))",
    )
    .bind(MEMBER_KEY)
    .bind(MEMBER_KEY)
    .execute(&mut *tx)
    .await
    .map_err(err)?;
    sqlx::query("DELETE FROM sync_outbox WHERE NOT EXISTS (SELECT 1 FROM sync_state WHERE key = ? AND value = '1')")
        .bind(MEMBER_KEY)
        .execute(&mut *tx)
        .await
        .map_err(err)?;
    tx.commit().await.map_err(err)
}

/// (Re)create the capture triggers when the registry changed since the last
/// install or some are missing. Returns whether they were rebuilt.
pub async fn install_triggers(db: &Db) -> Result<bool, String> {
    let err = |e: sqlx::Error| e.to_string();
    let hash = spec_hash();
    let expected = SYNCED_TABLES.len() as i64 * 3;
    let mut tx = db.begin().await.map_err(err)?;
    let stored: Option<String> = sqlx::query_scalar("SELECT value FROM sync_state WHERE key = ?")
        .bind(HASH_KEY)
        .fetch_optional(&mut *tx)
        .await
        .map_err(err)?;
    let present: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND name LIKE 'sync\\_cap\\_%' ESCAPE '\\'",
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(err)?;
    if stored.as_deref() == Some(hash.as_str()) && present == expected {
        return Ok(false);
    }
    let existing = sqlx::query(
        "SELECT name FROM sqlite_master WHERE type = 'trigger' AND name LIKE 'sync\\_cap\\_%' ESCAPE '\\'",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(err)?;
    for row in existing {
        let name: String = row.get(0);
        sqlx::query(&format!("DROP TRIGGER IF EXISTS {}", q(&name)))
            .execute(&mut *tx)
            .await
            .map_err(err)?;
    }
    for stmt in all_trigger_sql() {
        sqlx::query(&stmt).execute(&mut *tx).await.map_err(|e| format!("{e}: {stmt}"))?;
    }
    sqlx::query(
        "INSERT INTO sync_state (key, value) VALUES (?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(HASH_KEY)
    .bind(&hash)
    .execute(&mut *tx)
    .await
    .map_err(err)?;
    tx.commit().await.map_err(err)?;
    Ok(true)
}
