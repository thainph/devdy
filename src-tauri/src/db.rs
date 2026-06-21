use anyhow::Result;
use sqlx::{sqlite::SqlitePool, Pool, Sqlite};
use std::path::Path;

pub type Db = Pool<Sqlite>;

pub async fn init_db(db_path: &Path) -> Result<Db> {
    let db_url = format!("sqlite://{}?mode=rwc", db_path.to_string_lossy());
    let pool = SqlitePool::connect(&db_url).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    reconcile_interrupted_runs(&pool).await?;
    Ok(pool)
}

/// Runs marked `running` in the DB but with no live process — left behind when a
/// previous app session exited mid-run — are orphaned: the backend (and its
/// sidecars) died with the app. On a fresh start there's nothing still running,
/// so flip any leftover `running` rows to `cancelled` instead of leaving them
/// stuck forever.
async fn reconcile_interrupted_runs(pool: &Db) -> Result<()> {
    sqlx::query(
        "UPDATE runs SET status = 'cancelled', finished_at = ? \
         WHERE status = 'running'",
    )
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(())
}
