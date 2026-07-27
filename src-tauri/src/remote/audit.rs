//! Append-only audit log (DATA-003).
//!
//! The audit table is append-only: the app only ever INSERTs (BR-013 / SEC-007)
//! — never UPDATE/DELETE. The device registry was removed in the single-session
//! redesign; the `device_id` column remains in the schema but is unused (NULL).

use crate::db::Db;
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

/// Audit result values (DATA-003).
pub const RESULT_ACCEPTED: &str = "accepted";
pub const RESULT_REJECTED: &str = "rejected";

/// One audit entry (DATA-003).
#[derive(Debug, Serialize, Clone)]
pub struct AuditEntry {
    pub id: String,
    pub ts: String,
    pub device_id: Option<String>,
    pub room_id: Option<String>,
    pub run_id: Option<String>,
    pub action: String,
    pub result: String,
    pub reason: Option<String>,
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Append one audit entry (BR-013). NEVER fails the caller's flow — an audit
/// write error is logged best-effort but must not block command handling.
///
/// `device_id` is retained in the signature for schema compatibility but is
/// unused in the single-session redesign (callers pass `None`).
pub async fn audit(
    db: &Db,
    device_id: Option<&str>,
    room_id: Option<&str>,
    run_id: Option<&str>,
    action: &str,
    result: &str,
    reason: Option<&str>,
) {
    let id = Uuid::new_v4().to_string();
    let res = sqlx::query(
        "INSERT INTO remote_audit (id, ts, device_id, room_id, run_id, action, result, reason)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(now_rfc3339())
    .bind(device_id)
    .bind(room_id)
    .bind(run_id)
    .bind(action)
    .bind(result)
    .bind(reason)
    .execute(db)
    .await;
    if let Err(e) = res {
        // Metadata only — never a secret/plaintext payload.
        tracing::warn!(event = "remote_audit_write_failed", action = action, error = %e);
    }
}

/// Read the most recent audit entries (FR-011), newest first.
pub async fn list_audit(db: &Db, limit: i64) -> Result<Vec<AuditEntry>, String> {
    let rows = sqlx::query(
        "SELECT id, ts, device_id, room_id, run_id, action, result, reason
         FROM remote_audit ORDER BY ts DESC, id DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|r| AuditEntry {
            id: r.get("id"),
            ts: r.get("ts"),
            device_id: r.get("device_id"),
            room_id: r.get("room_id"),
            run_id: r.get("run_id"),
            action: r.get("action"),
            result: r.get("result"),
            reason: r.get("reason"),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn mem_db() -> Db {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE remote_audit (
                id TEXT PRIMARY KEY, ts TEXT NOT NULL, device_id TEXT, room_id TEXT,
                run_id TEXT, action TEXT NOT NULL, result TEXT NOT NULL, reason TEXT)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    // ---- AC-14: every command writes an audit row with full fields ----
    #[tokio::test]
    async fn audit_is_append_only_with_full_fields() {
        let db = mem_db().await;
        audit(
            &db,
            None,
            Some("rm_1"),
            Some("r_1"),
            "respond_permission",
            RESULT_ACCEPTED,
            None,
        )
        .await;
        audit(
            &db,
            None,
            Some("rm_1"),
            None,
            "start_run",
            RESULT_REJECTED,
            Some("wrong_run"),
        )
        .await;

        let entries = list_audit(&db, 50).await.unwrap();
        assert_eq!(entries.len(), 2, "both accepted and rejected are recorded");
        // Newest first: the rejected start_run.
        assert_eq!(entries[0].action, "start_run");
        assert_eq!(entries[0].result, RESULT_REJECTED);
        assert_eq!(entries[0].reason.as_deref(), Some("wrong_run"));
        // The accepted one carries a run_id and no reason.
        assert_eq!(entries[1].action, "respond_permission");
        assert_eq!(entries[1].result, RESULT_ACCEPTED);
        assert_eq!(entries[1].run_id.as_deref(), Some("r_1"));
        assert!(!entries[1].ts.is_empty());
    }
}
