use anyhow::Result;
use sqlx::{sqlite::SqlitePool, Pool, Sqlite};
use std::path::Path;

pub type Db = Pool<Sqlite>;

pub async fn init_db(db_path: &Path) -> Result<Db> {
    let db_url = format!("sqlite://{}?mode=rwc", db_path.to_string_lossy());
    let pool = SqlitePool::connect(&db_url).await?;
    drop_sync_triggers(&pool).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    reconcile_interrupted_runs(&pool).await?;
    // Best effort: a failed ledger repair must not keep the app from starting.
    if let Err(e) = crate::runs::usage_cost::repair_cumulative_costs(&pool).await {
        eprintln!("[db] run_usage cumulative-cost repair failed: {e}");
    }
    if let Err(e) = crate::runs::usage_cost::reprice_estimates(&pool).await {
        eprintln!("[db] run_usage estimate re-pricing failed: {e}");
    }
    Ok(pool)
}

/// Two-way sync (removed) installed change-capture triggers on most tables at
/// runtime, under dynamic names (`sync_cap_*`, `sync_past_owner_*`,
/// `sync_prefs_ref_*`). They write into the `sync_*` tables migration 0072
/// drops, so any left behind would break every insert/update on their table:
/// drop every `sync_*` trigger before migrating. No other trigger uses that
/// prefix. Idempotent, and a no-op on a fresh DB.
async fn drop_sync_triggers(pool: &Db) -> Result<()> {
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'trigger' AND name LIKE 'sync\\_%' ESCAPE '\\'",
    )
    .fetch_all(pool)
    .await?;
    for name in names {
        sqlx::query(&format!("DROP TRIGGER IF EXISTS \"{}\"", name.replace('"', "\"\"")))
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// Runs marked `running` in the DB but with no live process — left behind when a
/// previous app session exited mid-run. Nothing from that session is driving
/// them any more, so flip any leftover `running` rows to `cancelled` instead of
/// leaving them stuck forever. Their processes did NOT necessarily die with the
/// app: the background-process tracker (`procs::startup_sweep`) finds and stops
/// or lists whatever is still alive.
async fn reconcile_interrupted_runs(pool: &Db) -> Result<()> {
    sqlx::query("UPDATE runs SET status = 'cancelled', finished_at = ? WHERE status = 'running'")
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn count(pool: &Db, sql: &str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
    }

    /// A DB that had sync installed (migrated up to 0071, a runtime capture
    /// trigger writing into a sync table) still accepts writes after startup,
    /// and only sync's own tables, triggers and settings are gone.
    #[tokio::test]
    async fn startup_removes_what_sync_installed() {
        let dir = std::env::temp_dir().join(format!("devdy-drop-sync-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("data.db");
        {
            let pool = SqlitePool::connect(&format!("sqlite://{}?mode=rwc", path.to_string_lossy())).await.unwrap();
            let mut before = sqlx::migrate!("./migrations");
            before.migrations = before.migrations.iter().filter(|m| m.version < 72).cloned().collect();
            before.run(&pool).await.unwrap();
            for sql in [
                "CREATE TRIGGER sync_cap_todos_ins AFTER INSERT ON todos BEGIN \
                 INSERT INTO sync_outbox (tbl, pk, op, changed_at) VALUES ('todos', NEW.id, 'I', 0); END",
                "INSERT OR REPLACE INTO settings (key, value) VALUES ('sync_paused', 'true'), ('sync_device_name:abc', 'mini'), ('theme', 'dark')",
            ] {
                sqlx::query(sql).execute(&pool).await.unwrap();
            }
            pool.close().await;
        }

        let pool = init_db(&path).await.unwrap();
        sqlx::query("INSERT INTO todos (id, text, done, position, created_at, updated_at) VALUES ('a', 'x', 0, 0, 'c', 'u')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO projects (id, name, path, created_at) VALUES ('p1', 'P', '/tmp/p1', 'x')")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'sync\\_%' ESCAPE '\\' AND name != 'sync_conflicts'").await, 0);
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('sync_conflicts', 'rule_sync_conflicts')").await, 2);
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND name = 'trg_run_usage_claude_account'").await, 1);
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM settings WHERE key LIKE 'sync_%'").await, 0);
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM settings WHERE key = 'theme'").await, 1);
        pool.close().await;

        // Idempotent: a second startup finds nothing left to drop.
        init_db(&path).await.unwrap().close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
