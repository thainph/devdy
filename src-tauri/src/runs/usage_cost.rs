//! Per-turn cost from the Claude SDK's cumulative `total_cost_usd`.
//!
//! Since @anthropic-ai/claude-agent-sdk 0.3.288 every `result` event carries the
//! session's RUNNING cost total — across turns, and across resumes (a resumed
//! session continues from the total its transcript saved). Each Devdy turn is a
//! fresh resumed process, so summing those figures counts every earlier turn
//! again. The ledger (`run_usage.cost_usd`) must stay per-turn because every
//! consumer SUMs it, so a turn's cost is the difference from the previous
//! running total of the same run, kept in `run_usage.cost_cumulative_usd`.

use crate::runs::pricing;
use sqlx::{Row, SqlitePool};

/// Latest ledger row of a run: `None` when the run has no rows yet;
/// `Some(None)` when that row carries no running total (estimated, legacy
/// per-turn or synthetic rebuild row); `Some(Some(total))` otherwise.
pub type PrevTotal = Option<Option<f64>>;

/// Per-turn cost for a result reporting running total `cumulative`.
/// Returns `(cost, estimated)`.
///
/// * previous running total known → the difference; a total that went DOWN
///   means the SDK started a new one (`/clear`, a transcript without a saved
///   total), so the whole figure is this turn's.
/// * no rows yet → the run's first turn: the total IS its cost.
/// * earlier rows but no running total → those rows already cover the earlier
///   turns and there's no baseline to subtract, so estimate from this turn's
///   tokens instead of counting the history twice.
pub fn turn_cost(prev: PrevTotal, cumulative: f64, turn_estimate: f64) -> (f64, bool) {
    match prev {
        Some(Some(p)) if cumulative >= p => (cumulative - p, false),
        Some(Some(_)) | None => (cumulative, false),
        Some(None) => (turn_estimate, true),
    }
}

pub async fn previous_total(db: &SqlitePool, run_id: &str) -> PrevTotal {
    sqlx::query("SELECT cost_cumulative_usd FROM run_usage WHERE run_id = ? ORDER BY rowid DESC LIMIT 1")
        .bind(run_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .map(|row| row.try_get::<Option<f64>, _>("cost_cumulative_usd").ok().flatten())
}

/// Whether a legacy row's `cost` reads as a running total (given the previous
/// raw figure of the same run) rather than a per-turn cost: whichever
/// interpretation lands closer to the (calibrated) token-based estimate, in log
/// space. A token-less row with a non-decreasing figure is a repeat of the total.
fn reads_as_cumulative(cost: f64, prev_raw: Option<f64>, turn_estimate: f64) -> bool {
    let Some(prev) = prev_raw else { return false };
    if cost < prev {
        return false;
    }
    if turn_estimate <= 0.0 {
        return true;
    }
    let delta = cost - prev;
    if delta <= 0.0 {
        // Same total again: only a repeat if this turn barely cost anything.
        return cost > turn_estimate * 4.0;
    }
    let dist = |x: f64| (x / turn_estimate).ln().abs();
    dist(delta) < dist(cost)
}

/// Where a run's running-total rows begin, given its raw costs in order and a
/// per-row estimate. The first row that reads as a running total proves the
/// switch, but the earliest totals of a session look like per-turn costs, so
/// walk back to the start of the non-decreasing streak it belongs to.
/// `None` when no row reads as a running total.
fn cumulative_start(costs: &[f64], estimates: &[f64]) -> Option<usize> {
    let first = (1..costs.len())
        .find(|&i| reads_as_cumulative(costs[i], Some(costs[i - 1]), estimates[i]))?;
    let mut start = first;
    while start > 0 && costs[start - 1] <= costs[start] {
        start -= 1;
    }
    Some(start)
}

/// Per-turn costs for a slice of running totals (a drop starts a new total).
fn differences(totals: &[f64]) -> Vec<f64> {
    let mut prev: Option<f64> = None;
    totals
        .iter()
        .map(|&c| {
            let d = match prev {
                Some(p) if c >= p => c - p,
                _ => c,
            };
            prev = Some(c);
            d
        })
        .collect()
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    Some(v[v.len() / 2])
}

/// Earliest moment the cumulative SDK could have run (the bump commit). Rows
/// before it are always per-turn and are left alone.
const CUMULATIVE_SINCE: &str = "2026-10-03T03:56:00+00:00";
const REPAIR_KEY: &str = "run_usage_cost_cumulative_repaired_v1";

struct LedgerRow {
    id: String,
    run_id: String,
    model: String,
    cost: f64,
    estimate: f64,
}

async fn load_rows(db: &SqlitePool, since: bool) -> anyhow::Result<Vec<LedgerRow>> {
    let cmp = if since { ">=" } else { "<" };
    let rows = sqlx::query(&format!(
        "SELECT id, run_id, model, input_tokens, output_tokens, cache_creation_tokens, \
         cache_read_tokens, cost_usd FROM run_usage \
         WHERE engine = 'claude' AND cost_estimated = 0 AND cost_usd IS NOT NULL \
           AND run_id IS NOT NULL AND cost_cumulative_usd IS NULL \
           AND julianday(created_at) {cmp} julianday(?) \
         ORDER BY run_id, rowid"
    ))
    .bind(CUMULATIVE_SINCE)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            let model: String = r.get::<Option<String>, _>("model").unwrap_or_default();
            let estimate = pricing::estimate_cost(
                &model,
                r.get("input_tokens"),
                r.get("output_tokens"),
                r.get("cache_creation_tokens"),
                r.get("cache_read_tokens"),
            );
            LedgerRow { id: r.get("id"), run_id: r.get("run_id"), model, cost: r.get("cost_usd"), estimate }
        })
        .collect())
}

/// One-time repair of rows recorded as running totals before `turn_cost`
/// existed. The pricing table is off by a roughly constant factor per model, so
/// estimates are first calibrated against the rows that are certainly per-turn
/// (before `CUMULATIVE_SINCE`). Then, per run, rows from `cumulative_start` on
/// are rewritten to their per-turn difference and keep the raw running total;
/// a run with no running totals only gets its last raw figure as a baseline so
/// the next live turn can difference against it.
pub async fn repair_cumulative_costs(db: &SqlitePool) -> anyhow::Result<()> {
    let done: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(REPAIR_KEY)
        .fetch_optional(db)
        .await?;
    if done.is_some() {
        return Ok(());
    }

    let mut ratios: std::collections::HashMap<String, Vec<f64>> = Default::default();
    for r in load_rows(db, false).await? {
        if r.estimate > 0.0 && r.cost > 0.0 {
            ratios.entry(r.model).or_default().push(r.cost / r.estimate);
        }
    }
    let global = median(ratios.values().flatten().copied().collect()).unwrap_or(1.0);
    let per_model: std::collections::HashMap<String, f64> = ratios
        .into_iter()
        .filter(|(_, v)| v.len() >= 20)
        .filter_map(|(m, v)| median(v).map(|x| (m, x)))
        .collect();

    let rows = load_rows(db, true).await?;
    let mut tx = db.begin().await?;
    for run in rows.chunk_by(|a, b| a.run_id == b.run_id) {
        let costs: Vec<f64> = run.iter().map(|r| r.cost).collect();
        let estimates: Vec<f64> = run
            .iter()
            .map(|r| r.estimate * per_model.get(&r.model).copied().unwrap_or(global))
            .collect();
        let Some(start) = cumulative_start(&costs, &estimates) else {
            let last = run.last().expect("chunks are non-empty");
            sqlx::query("UPDATE run_usage SET cost_cumulative_usd = cost_usd WHERE id = ?")
                .bind(&last.id)
                .execute(&mut *tx)
                .await?;
            continue;
        };
        for (r, d) in run[start..].iter().zip(differences(&costs[start..])) {
            sqlx::query("UPDATE run_usage SET cost_usd = ?, cost_cumulative_usd = ? WHERE id = ?")
                .bind(d)
                .bind(r.cost)
                .bind(&r.id)
                .execute(&mut *tx)
                .await?;
        }
    }
    sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)")
        .bind(REPAIR_KEY)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

const PRICING_KEY: &str = "run_usage_estimates_pricing_version";

/// Re-price stored estimated rows (`cost_estimated = 1`) whenever the pricing
/// table changes (`pricing::PRICING_VERSION`), so old and new estimates agree.
/// Runs once per table version.
pub async fn reprice_estimates(db: &SqlitePool) -> anyhow::Result<()> {
    let stored: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(PRICING_KEY)
        .fetch_optional(db)
        .await?;
    if stored.as_deref() == Some(pricing::PRICING_VERSION) {
        return Ok(());
    }
    let rows = sqlx::query(
        "SELECT id, model, input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens \
         FROM run_usage WHERE cost_estimated = 1",
    )
    .fetch_all(db)
    .await?;
    let mut tx = db.begin().await?;
    for r in &rows {
        let cost = pricing::estimate_cost(
            r.get::<Option<String>, _>("model").as_deref().unwrap_or(""),
            r.get("input_tokens"),
            r.get("output_tokens"),
            r.get("cache_creation_tokens"),
            r.get("cache_read_tokens"),
        );
        sqlx::query("UPDATE run_usage SET cost_usd = ? WHERE id = ?")
            .bind(cost)
            .bind(r.get::<String, _>("id"))
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)")
        .bind(PRICING_KEY)
        .bind(pricing::PRICING_VERSION)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_cost_differences_running_total() {
        assert_eq!(turn_cost(Some(Some(316.3)), 316.65, 0.9), (316.65 - 316.3, false));
    }

    #[test]
    fn turn_cost_treats_drop_as_new_total() {
        assert_eq!(turn_cost(Some(Some(12.0)), 0.4, 0.9), (0.4, false));
    }

    #[test]
    fn turn_cost_first_row_is_whole_total() {
        assert_eq!(turn_cost(None, 1.2, 0.9), (1.2, false));
    }

    #[test]
    fn turn_cost_without_baseline_estimates() {
        assert_eq!(turn_cost(Some(None), 316.65, 0.9), (0.9, true));
    }

    #[test]
    fn classifies_running_totals() {
        // Observed: totals 316.313 → 316.649 for a ~1.57M-token (mostly cache) turn.
        assert!(reads_as_cumulative(316.649, Some(316.313), 0.9));
        // Token-less repeat of the total.
        assert!(reads_as_cumulative(17.713, Some(17.713), 0.0));
    }

    #[test]
    fn keeps_per_turn_costs() {
        // First row of a run, or a drop: always per-turn.
        assert!(!reads_as_cumulative(0.169, None, 0.15));
        assert!(!reads_as_cumulative(0.336, Some(0.549), 0.3));
        // Equal cost on a turn that really cost that much.
        assert!(!reads_as_cumulative(0.5, Some(0.5), 0.45));
    }

    #[test]
    fn start_walks_back_over_early_totals() {
        // Per-turn rows (0.9, 0.4), then a fresh running total that grows. The
        // second total (0.5) still looks per-turn; 7.6 vs ~0.9 proves the switch.
        let costs = [0.9, 0.4, 0.17, 0.5, 7.6, 8.5];
        let est = [0.9, 0.4, 0.17, 0.33, 0.95, 0.9];
        assert_eq!(cumulative_start(&costs, &est), Some(2));
    }

    #[test]
    fn no_start_for_per_turn_runs() {
        assert_eq!(cumulative_start(&[0.9, 0.4, 1.1, 0.7], &[0.9, 0.4, 1.1, 0.7]), None);
        assert_eq!(cumulative_start(&[1.2], &[1.0]), None);
    }

    #[test]
    fn differences_reset_on_drop() {
        let d = differences(&[0.17, 0.5, 7.6, 0.3, 0.8]);
        let want = [0.17, 0.33, 7.1, 0.3, 0.5];
        for (a, b) in d.iter().zip(want) {
            assert!((a - b).abs() < 1e-9, "{d:?}");
        }
    }

    async fn ledger_db() -> SqlitePool {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&db).await.unwrap();
        db
    }

    async fn insert(db: &SqlitePool, id: &str, run: &str, at: &str, cache_read: i64, cost: f64) {
        sqlx::query(
            "INSERT INTO run_usage (id, run_id, engine, model, cache_read_tokens, total_tokens, \
             cost_usd, created_at) VALUES (?, ?, 'claude', 'claude-opus-5', ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(run)
        .bind(cache_read)
        .bind(cache_read)
        .bind(cost)
        .bind(at)
        .execute(db)
        .await
        .unwrap();
    }

    async fn costs(db: &SqlitePool, run: &str) -> Vec<(f64, Option<f64>)> {
        sqlx::query_as("SELECT cost_usd, cost_cumulative_usd FROM run_usage WHERE run_id = ? ORDER BY rowid")
            .bind(run)
            .fetch_all(db)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn repair_rewrites_running_totals_once() {
        let db = ledger_db().await;
        // Per-turn history before the cutoff calibrates estimates: opus cache
        // read is listed at $1.5/M, these turns really cost ~$0.5/M.
        for i in 0..30 {
            insert(&db, &format!("old{i}"), "calib", "2026-09-20T00:00:00+00:00", 1_000_000, 0.5).await;
        }
        // Run A: two per-turn rows before the cutoff, then running totals.
        insert(&db, "a1", "A", "2026-10-01T00:00:00+00:00", 1_000_000, 0.6).await;
        insert(&db, "a2", "A", "2026-10-02T00:00:00+00:00", 1_000_000, 0.4).await;
        insert(&db, "a3", "A", "2026-10-04T00:00:00+00:00", 1_000_000, 0.5).await;
        insert(&db, "a4", "A", "2026-10-04T01:00:00+00:00", 1_000_000, 1.0).await;
        insert(&db, "a5", "A", "2026-10-04T02:00:00+00:00", 2_000_000, 2.0).await;
        // Run B: a single post-cutoff row — untouched, but becomes the baseline.
        insert(&db, "b1", "B", "2026-10-05T00:00:00+00:00", 1_000_000, 0.5).await;

        repair_cumulative_costs(&db).await.unwrap();
        assert_eq!(
            costs(&db, "A").await,
            vec![(0.6, None), (0.4, None), (0.5, Some(0.5)), (0.5, Some(1.0)), (1.0, Some(2.0))]
        );
        assert_eq!(costs(&db, "B").await, vec![(0.5, Some(0.5))]);

        // Guarded by the settings marker: a second pass changes nothing.
        insert(&db, "a6", "A", "2026-10-06T00:00:00+00:00", 1_000_000, 2.5).await;
        repair_cumulative_costs(&db).await.unwrap();
        assert_eq!(costs(&db, "A").await.last(), Some(&(2.5, None)));
    }
}
