use anyhow::Result;
use sqlx::{sqlite::SqlitePool, Pool, Sqlite};
use std::path::Path;

pub type Db = Pool<Sqlite>;

pub async fn init_db(db_path: &Path) -> Result<Db> {
    let db_url = format!("sqlite://{}?mode=rwc", db_path.to_string_lossy());
    let pool = SqlitePool::connect(&db_url).await?;
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

/// Runs marked `running` in the DB but with no live process — left behind when a
/// previous app session exited mid-run. Nothing from that session is driving
/// them any more, so flip any leftover `running` rows to `cancelled` instead of
/// leaving them stuck forever. Their processes did NOT necessarily die with the
/// app: the background-process tracker (`procs::startup_sweep`) finds and stops
/// or lists whatever is still alive. Runs another device is running (synced)
/// are left alone.
async fn reconcile_interrupted_runs(pool: &Db) -> Result<()> {
    crate::sync::sessions::reconcile_startup(pool).await?;
    Ok(())
}
