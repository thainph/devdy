//! Per-turn, per-model usage from the Claude SDK's `modelUsage`.
//!
//! A `result` event's `usage` covers the MAIN AGENT LOOP of that turn only —
//! Task subagents, sidechains and compaction calls are missing — and it carries
//! a single model. `modelUsage` is the complete picture: per model, the running
//! totals (tokens + `costUSD`) of every call in the session. It is cumulative
//! within a CLI process and, since CLI 2.1.288, across resumes too (the total
//! continues from what the transcript saved). Each Devdy turn is a resumed
//! process, so a turn's usage is the per-model difference from the previous
//! result of the same run; that previous `modelUsage` is the run's baseline
//! (`run_usage_baseline`). The ledger gets one row per model used in the turn.
//!
//! Older CLIs (2.1.268 and before) reset the totals in every process, so their
//! one-result-per-process figures already are per-turn. Run logs record the CLI
//! version on `system.init`, which lets `turns_from_log` tell the two apart
//! exactly — the one-time `rebuild_from_logs` re-derives the ledger from logs.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use std::collections::BTreeMap;

/// First CLI whose running totals carry over resumes (bundled with
/// @anthropic-ai/claude-agent-sdk 0.3.288).
const CARRY_OVER_SINCE: (u64, u64, u64) = (2, 1, 288);

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
    pub cost: f64,
}

impl Usage {
    pub fn total_tokens(&self) -> i64 {
        self.input + self.output + self.cache_read + self.cache_creation
    }

    fn is_zero(&self) -> bool {
        self.total_tokens() == 0 && self.cost == 0.0
    }

    /// `self - prev` when this continues `prev`'s running total, `None` when
    /// it restarted. The cost decides: token counters restored on resume can
    /// come back slightly lower (seen: outputTokens −0.3%) while the cost keeps
    /// growing, so tokens are differenced saturating instead.
    fn minus(&self, prev: &Usage) -> Option<Usage> {
        // costUSD is a float sum; tolerate rounding noise.
        let cost = self.cost - prev.cost;
        if cost < -1e-9 {
            return None;
        }
        Some(Usage {
            input: (self.input - prev.input).max(0),
            output: (self.output - prev.output).max(0),
            cache_read: (self.cache_read - prev.cache_read).max(0),
            cache_creation: (self.cache_creation - prev.cache_creation).max(0),
            cost: cost.max(0.0),
        })
    }
}

/// Model id → usage. Ordered so ledger rows are written deterministically.
pub type ModelUsage = BTreeMap<String, Usage>;

/// Read `modelUsage` from a `result` event. `None` when absent or all-zero
/// (crash / startup-error results carry zeroed totals that must not become a
/// baseline).
pub fn parse_model_usage(v: &Value) -> Option<ModelUsage> {
    let obj = v.get("modelUsage")?.as_object()?;
    let num = |m: &Value, k: &str| m.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
    let map: ModelUsage = obj
        .iter()
        .map(|(model, m)| {
            let u = Usage {
                input: num(m, "inputTokens"),
                output: num(m, "outputTokens"),
                cache_read: num(m, "cacheReadInputTokens"),
                cache_creation: num(m, "cacheCreationInputTokens"),
                cost: m.get("costUSD").and_then(|x| x.as_f64()).unwrap_or(0.0),
            };
            (model.clone(), u)
        })
        .collect();
    if map.values().all(Usage::is_zero) {
        return None;
    }
    Some(map)
}

/// Per-model difference `cur - prev`, dropping models with nothing new. Each
/// model is judged on its own: one whose total went down restarted, so its
/// whole current figure is new; a model missing from `cur` is simply ignored.
pub fn diff(cur: &ModelUsage, prev: &ModelUsage) -> ModelUsage {
    let mut out = ModelUsage::new();
    for (model, u) in cur {
        let d = prev.get(model).and_then(|p| u.minus(p)).unwrap_or(*u);
        if !d.is_zero() {
            out.insert(model.clone(), d);
        }
    }
    out
}

/// This turn's usage given the current totals and the previous result's, when
/// `cumulative` says the two belong to the same running total.
pub fn turn_usage(cur: &ModelUsage, prev: Option<&ModelUsage>, cumulative: bool) -> ModelUsage {
    let fresh = || cur.iter().filter(|(_, u)| !u.is_zero()).map(|(m, u)| (m.clone(), *u)).collect();
    match prev {
        Some(p) if cumulative => diff(cur, p),
        _ => fresh(),
    }
}

/// Whether a CLI version's running totals continue across resumes.
pub fn carries_over(version: Option<&str>) -> bool {
    let Some(v) = version else { return false };
    let mut parts = v.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty());
    let mut next = || parts.next().and_then(|p| p.parse::<u64>().ok());
    match (next(), next(), next()) {
        (Some(a), Some(b), Some(c)) => (a, b, c) >= CARRY_OVER_SINCE,
        _ => false,
    }
}

/// One `result` of a run log, reduced to what the ledger needs.
#[derive(Debug, Clone, PartialEq)]
pub struct LogTurn {
    /// This turn's per-model usage (may be empty when nothing new happened).
    pub per_model: ModelUsage,
    /// Raw `total_cost_usd` (the running total on carry-over CLIs).
    pub total_cost: Option<f64>,
    pub num_turns: Option<i64>,
    pub duration_ms: Option<i64>,
    /// Whether the pre-`modelUsage` ledger wrote a row for this result (it
    /// skipped results with no main-loop tokens and no cost). Used to line log
    /// results up with existing rows.
    pub legacy_row: bool,
}

/// The run's baseline after the last result of a log: its raw totals and
/// whether they carry over into a resumed turn.
#[derive(Debug, Clone, PartialEq)]
pub struct Baseline {
    pub usage: ModelUsage,
    pub carries_over: bool,
}

fn main_loop_tokens(v: &Value) -> i64 {
    let u = v.get("usage");
    let tok = |k: &str| u.and_then(|u| u.get(k)).and_then(|x| x.as_i64()).unwrap_or(0);
    tok("input_tokens")
        + tok("output_tokens")
        + tok("cache_creation_input_tokens")
        + tok("cache_read_input_tokens")
}

/// Derive per-turn usage from a Claude run log (stream-json lines). A result is
/// cumulative with the previous one when both sit in the same CLI process, or
/// when both processes ran a carry-over CLI (a resume of a transcript written
/// by an older CLI starts a fresh total).
pub fn turns_from_log(content: &str) -> (Vec<LogTurn>, Option<Baseline>) {
    let mut version: Option<String> = None;
    let mut in_process = 0usize;
    let mut prev: Option<Baseline> = None;
    let mut turns = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        match v.get("type").and_then(|x| x.as_str()) {
            Some("system") if v.get("subtype").and_then(|x| x.as_str()) == Some("init") => {
                version = v.get("claude_code_version").and_then(|x| x.as_str()).map(str::to_string);
                in_process = 0;
            }
            Some("result") => {
                let total_cost = v.get("total_cost_usd").and_then(|x| x.as_f64());
                let legacy_row = main_loop_tokens(&v) != 0 || total_cost.unwrap_or(0.0) != 0.0;
                let ver_carries = carries_over(version.as_deref());
                let per_model = match parse_model_usage(&v) {
                    Some(cur) => {
                        let cumulative = in_process > 0
                            || (ver_carries && prev.as_ref().is_some_and(|p| p.carries_over));
                        let t = turn_usage(&cur, prev.as_ref().map(|p| &p.usage), cumulative);
                        prev = Some(Baseline { usage: cur, carries_over: ver_carries });
                        t
                    }
                    None => ModelUsage::new(),
                };
                turns.push(LogTurn {
                    per_model,
                    total_cost,
                    num_turns: v.get("num_turns").and_then(|x| x.as_i64()),
                    duration_ms: v.get("duration_ms").and_then(|x| x.as_i64()),
                    legacy_row,
                });
                in_process += 1;
            }
            _ => {}
        }
    }
    (turns, prev)
}

// ── DB ────────────────────────────────────────────────────────────────────

pub async fn load_baseline(db: &SqlitePool, run_id: &str) -> Option<Baseline> {
    let row = sqlx::query("SELECT model_usage, carries_over FROM run_usage_baseline WHERE run_id = ?")
        .bind(run_id)
        .fetch_optional(db)
        .await
        .ok()??;
    let usage: ModelUsage = serde_json::from_str(&row.get::<String, _>("model_usage")).ok()?;
    Some(Baseline { usage, carries_over: row.get::<i64, _>("carries_over") != 0 })
}

pub async fn save_baseline(
    tx: &mut Transaction<'_, Sqlite>,
    run_id: &str,
    b: &Baseline,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO run_usage_baseline (run_id, model_usage, carries_over, updated_at) \
         VALUES (?, ?, ?, ?) ON CONFLICT(run_id) DO UPDATE SET \
         model_usage = excluded.model_usage, carries_over = excluded.carries_over, \
         updated_at = excluded.updated_at",
    )
    .bind(run_id)
    .bind(serde_json::to_string(&b.usage).unwrap_or_else(|_| "{}".into()))
    .bind(b.carries_over as i64)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(&mut **tx)
    .await
    .map(|_| ())
}

/// Ledger identity shared by every row of one turn.
pub struct RowMeta<'a> {
    pub run_id: &'a str,
    pub project_id: &'a str,
    pub project_name: &'a str,
    pub engine: &'a str,
    pub created_at: &'a str,
    pub deleted_run: bool,
}

/// Write one turn as one ledger row per model. Turn-level figures (`num_turns`,
/// `duration_ms`) go on the costliest model's row so SUMs stay per-turn.
/// Returns the number of rows written.
pub async fn insert_turn_rows(
    tx: &mut Transaction<'_, Sqlite>,
    meta: &RowMeta<'_>,
    per_model: &ModelUsage,
    total_cost: Option<f64>,
    num_turns: Option<i64>,
    duration_ms: Option<i64>,
) -> sqlx::Result<usize> {
    let main = per_model
        .iter()
        .max_by(|a, b| a.1.cost.total_cmp(&b.1.cost).then(a.1.total_tokens().cmp(&b.1.total_tokens())))
        .map(|(m, _)| m.clone());
    for (model, u) in per_model {
        let is_main = main.as_deref() == Some(model.as_str());
        sqlx::query(
            "INSERT INTO run_usage (
                id, run_id, project_id, project_name, engine, model,
                input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
                total_tokens, cost_usd, cost_cumulative_usd, cost_estimated, num_turns,
                duration_ms, created_at, deleted_run
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?, ?, ?)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(meta.run_id)
        .bind(meta.project_id)
        .bind(meta.project_name)
        .bind(meta.engine)
        .bind(model)
        .bind(u.input)
        .bind(u.output)
        .bind(u.cache_creation)
        .bind(u.cache_read)
        .bind(u.total_tokens())
        .bind(u.cost)
        .bind(total_cost)
        .bind(if is_main { num_turns } else { None })
        .bind(if is_main { duration_ms } else { None })
        .bind(meta.created_at)
        .bind(meta.deleted_run as i64)
        .execute(&mut **tx)
        .await?;
    }
    Ok(per_model.len())
}

/// Live turn: this result's per-model usage, differenced against the run's
/// baseline. `results_in_process` counts earlier results of the same CLI
/// process. `None` when the run has ledger rows but no baseline (rows from
/// before `modelUsage` was tracked, or a transcript rebuild): there's nothing
/// to difference against, so the caller records the turn the legacy way.
pub async fn live_turn_usage(
    db: &SqlitePool,
    run_id: &str,
    cur: &ModelUsage,
    cli_version: Option<&str>,
    results_in_process: usize,
) -> Option<ModelUsage> {
    if let Some(b) = load_baseline(db, run_id).await {
        let cumulative = results_in_process > 0 || (carries_over(cli_version) && b.carries_over);
        return Some(turn_usage(cur, Some(&b.usage), cumulative));
    }
    let has_rows: Option<i64> = sqlx::query_scalar("SELECT 1 FROM run_usage WHERE run_id = ? LIMIT 1")
        .bind(run_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
    has_rows.is_none().then(|| turn_usage(cur, None, false))
}

/// Persist a live turn: its per-model rows (when given) and the new baseline,
/// atomically. Returns the number of rows written.
pub async fn commit_live_turn(
    db: &SqlitePool,
    meta: &RowMeta<'_>,
    per_model: Option<&ModelUsage>,
    result: &Value,
    baseline: &Baseline,
) -> anyhow::Result<usize> {
    let mut tx = db.begin().await?;
    let n = match per_model {
        Some(pm) => {
            insert_turn_rows(
                &mut tx,
                meta,
                pm,
                result.get("total_cost_usd").and_then(|x| x.as_f64()),
                result.get("num_turns").and_then(|x| x.as_i64()),
                result.get("duration_ms").and_then(|x| x.as_i64()),
            )
            .await?
        }
        None => 0,
    };
    save_baseline(&mut tx, meta.run_id, baseline).await?;
    tx.commit().await?;
    Ok(n)
}

/// Forget a run's baseline — when its ledger is replaced by a transcript
/// rebuild, the next live turn has to start over the legacy way.
pub async fn clear_baseline(db: &SqlitePool, run_id: &str) {
    let _ = sqlx::query("DELETE FROM run_usage_baseline WHERE run_id = ?")
        .bind(run_id)
        .execute(db)
        .await;
}

const REBUILD_KEY: &str = "run_usage_rebuilt_from_logs_v1";

/// One-time: re-derive every Claude run's ledger from its log, replacing the
/// main-loop-only, single-model rows (and the heuristic cumulative-cost repair)
/// with exact per-model rows. Log results are lined up with the existing rows
/// to keep each turn's timestamp; a run is left untouched when they don't line
/// up, when it has estimated rows (transcript rebuilds own those), when its log
/// is gone, or when it already has a baseline (live-captured since).
/// Resumable: rebuilt runs get a baseline, so a rerun skips them.
pub async fn rebuild_from_logs(db: &SqlitePool) -> anyhow::Result<()> {
    let done: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(REBUILD_KEY)
        .fetch_optional(db)
        .await?;
    if done.is_some() {
        return Ok(());
    }
    let runs = sqlx::query(
        "SELECT r.id, r.output_path FROM runs r \
         WHERE r.engine = 'claude' AND r.output_path IS NOT NULL \
           AND NOT EXISTS (SELECT 1 FROM run_usage_baseline b WHERE b.run_id = r.id)",
    )
    .fetch_all(db)
    .await?;

    for run in &runs {
        let run_id: String = run.get("id");
        let path: String = run.get("output_path");
        let Ok(content) = tokio::fs::read_to_string(&path).await else { continue };
        let (turns, baseline) = turns_from_log(&content);
        if let Err(e) = rebuild_run(db, &run_id, &turns, baseline.as_ref()).await {
            eprintln!("[usage] rebuild of run {run_id} failed: {e}");
        }
    }

    sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)")
        .bind(REBUILD_KEY)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(db)
        .await?;
    Ok(())
}

async fn rebuild_run(
    db: &SqlitePool,
    run_id: &str,
    turns: &[LogTurn],
    baseline: Option<&Baseline>,
) -> anyhow::Result<bool> {
    let mut tx = db.begin().await?;
    // Re-check inside the transaction: a live turn may have landed meanwhile.
    let has_baseline: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM run_usage_baseline WHERE run_id = ?")
            .bind(run_id)
            .fetch_optional(&mut *tx)
            .await?;
    if has_baseline.is_some() {
        return Ok(false);
    }
    let rows = sqlx::query(
        "SELECT project_id, project_name, engine, created_at, cost_estimated, deleted_run \
         FROM run_usage WHERE run_id = ? ORDER BY rowid",
    )
    .bind(run_id)
    .fetch_all(&mut *tx)
    .await?;
    let legacy: Vec<&LogTurn> = turns.iter().filter(|t| t.legacy_row).collect();
    if rows.is_empty()
        || rows.len() != legacy.len()
        || rows.iter().any(|r| r.get::<i64, _>("cost_estimated") != 0)
    {
        return Ok(false);
    }

    sqlx::query("DELETE FROM run_usage WHERE run_id = ?")
        .bind(run_id)
        .execute(&mut *tx)
        .await?;
    for (row, turn) in rows.iter().zip(legacy) {
        let project_id: Option<String> = row.get("project_id");
        let project_name: Option<String> = row.get("project_name");
        let engine: String = row.get("engine");
        let created_at: String = row.get("created_at");
        let meta = RowMeta {
            run_id,
            project_id: project_id.as_deref().unwrap_or(""),
            project_name: project_name.as_deref().unwrap_or(""),
            engine: &engine,
            created_at: &created_at,
            deleted_run: row.get::<i64, _>("deleted_run") != 0,
        };
        insert_turn_rows(&mut tx, &meta, &turn.per_model, turn.total_cost, turn.num_turns, turn.duration_ms)
            .await?;
    }
    if let Some(b) = baseline {
        save_baseline(&mut tx, run_id, b).await?;
    }
    tx.commit().await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mu(entries: &[(&str, i64, f64)]) -> ModelUsage {
        entries
            .iter()
            .map(|(m, tok, cost)| {
                (m.to_string(), Usage { input: 0, output: 0, cache_read: *tok, cache_creation: 0, cost: *cost })
            })
            .collect()
    }

    #[test]
    fn version_threshold() {
        assert!(!carries_over(Some("2.1.268")));
        assert!(!carries_over(Some("2.1.179")));
        assert!(carries_over(Some("2.1.288")));
        assert!(carries_over(Some("2.2.0")));
        assert!(carries_over(Some("3.0.1-beta")));
        assert!(!carries_over(None));
        assert!(!carries_over(Some("garbage")));
    }

    #[test]
    fn diff_per_model_and_new_models() {
        let prev = mu(&[("opus", 100, 1.0)]);
        let cur = mu(&[("opus", 150, 1.5), ("haiku", 40, 0.1)]);
        assert_eq!(diff(&cur, &prev), mu(&[("haiku", 40, 0.1), ("opus", 50, 0.5)]));
    }

    #[test]
    fn diff_restarts_per_model() {
        // Opus restarted (cost went down); haiku kept growing.
        let prev = mu(&[("opus", 100, 1.0), ("haiku", 5, 0.01)]);
        let cur = mu(&[("opus", 10, 0.1), ("haiku", 8, 0.02)]);
        let d = diff(&cur, &prev);
        assert_eq!(d["opus"], mu(&[("opus", 10, 0.1)])["opus"]);
        assert_eq!(d["haiku"].cache_read, 3);
        // A model missing from the current totals is ignored.
        assert_eq!(diff(&mu(&[("opus", 200, 2.0)]), &mu(&[("opus", 100, 1.0), ("haiku", 5, 0.01)])), mu(&[("opus", 100, 1.0)]));
    }

    #[test]
    fn diff_tolerates_token_counters_dipping() {
        // Observed on resume: outputTokens restored slightly lower, cost still up.
        let prev = [("opus".to_string(), Usage { output: 766_179, cost: 300.0, ..Default::default() })].into();
        let cur = [("opus".to_string(), Usage { output: 764_193, cost: 300.4, ..Default::default() })].into();
        let d = diff(&cur, &prev);
        assert_eq!(d["opus"].output, 0);
        assert!((d["opus"].cost - 0.4).abs() < 1e-9);
    }

    #[test]
    fn turn_usage_falls_back_to_fresh_total() {
        let prev = mu(&[("opus", 100, 1.0)]);
        let cur = mu(&[("opus", 30, 0.3)]);
        assert_eq!(turn_usage(&cur, Some(&prev), true), cur);
        assert_eq!(turn_usage(&mu(&[("opus", 130, 1.3)]), Some(&prev), false), mu(&[("opus", 130, 1.3)]));
    }

    #[test]
    fn parse_skips_zeroed_results() {
        let v: Value = serde_json::json!({"type": "result", "modelUsage": {"opus": {"inputTokens": 0, "costUSD": 0.0}}});
        assert_eq!(parse_model_usage(&v), None);
        let v: Value = serde_json::json!({"type": "result", "modelUsage": {
            "opus": {"inputTokens": 1, "outputTokens": 2, "cacheReadInputTokens": 3, "cacheCreationInputTokens": 4, "costUSD": 0.5}
        }});
        let u = parse_model_usage(&v).unwrap()["opus"];
        assert_eq!((u.input, u.output, u.cache_read, u.cache_creation, u.cost), (1, 2, 3, 4, 0.5));
    }

    fn init(ver: &str) -> String {
        format!(r#"{{"type":"system","subtype":"init","claude_code_version":"{ver}"}}"#)
    }
    fn result(tok: i64, total: f64, main_tok: i64) -> String {
        format!(
            r#"{{"type":"result","total_cost_usd":{total},"num_turns":1,"usage":{{"input_tokens":{main_tok}}},"modelUsage":{{"opus":{{"cacheReadInputTokens":{tok},"costUSD":{total}}}}}}}"#
        )
    }

    #[test]
    fn log_old_cli_is_per_turn_new_cli_differences() {
        let log = [
            init("2.1.268"),
            result(100, 1.0, 10),
            init("2.1.268"),
            result(150, 1.5, 10), // higher by chance — still per-turn
            init("2.1.288"),
            result(40, 0.4, 10), // resume of an old transcript: fresh total
            init("2.1.288"),
            result(70, 0.7, 10), // carries over: +30
            "[stderr noise]".to_string(),
            init("2.1.288"),
            result(20, 0.2, 10), // went down: /clear — fresh
        ]
        .join("\n");
        let (turns, base) = turns_from_log(&log);
        let got: Vec<ModelUsage> = turns.iter().map(|t| t.per_model.clone()).collect();
        let want = vec![
            mu(&[("opus", 100, 1.0)]),
            mu(&[("opus", 150, 1.5)]),
            mu(&[("opus", 40, 0.4)]),
            mu(&[("opus", 30, 0.7 - 0.4)]),
            mu(&[("opus", 20, 0.2)]),
        ];
        for (g, w) in got.iter().zip(&want) {
            let (g, w) = (g["opus"], w["opus"]);
            assert_eq!(g.cache_read, w.cache_read);
            assert!((g.cost - w.cost).abs() < 1e-9);
        }
        assert_eq!(base, Some(Baseline { usage: mu(&[("opus", 20, 0.2)]), carries_over: true }));
    }

    #[test]
    fn log_results_in_one_process_are_cumulative_on_any_cli() {
        let log = [init("2.1.179"), result(100, 1.0, 10), result(160, 1.6, 10)].join("\n");
        let (turns, _) = turns_from_log(&log);
        assert_eq!(turns[1].per_model["opus"].cache_read, 60);
    }

    #[test]
    fn legacy_row_flag_mirrors_old_skip_rule() {
        let log = [
            init("2.1.288"),
            r#"{"type":"result","total_cost_usd":0,"usage":{"input_tokens":0}}"#.to_string(),
            result(10, 0.1, 0),
        ]
        .join("\n");
        let (turns, _) = turns_from_log(&log);
        assert_eq!(turns.iter().map(|t| t.legacy_row).collect::<Vec<_>>(), vec![false, true]);
    }

    async fn db() -> SqlitePool {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&db).await.unwrap();
        db
    }

    async fn legacy_row(db: &SqlitePool, run: &str, at: &str, cost: f64) {
        sqlx::query(
            "INSERT INTO run_usage (id, run_id, project_id, project_name, engine, model, total_tokens, \
             cost_usd, created_at) VALUES (?, ?, 'p', 'proj', 'claude', 'opus', 10, ?, ?)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(run)
        .bind(cost)
        .bind(at)
        .execute(db)
        .await
        .unwrap();
    }

    async fn rows(db: &SqlitePool, run: &str) -> Vec<(String, String, i64, f64)> {
        sqlx::query_as(
            "SELECT created_at, model, total_tokens, cost_usd FROM run_usage WHERE run_id = ? ORDER BY rowid",
        )
        .bind(run)
        .fetch_all(db)
        .await
        .unwrap()
    }

    fn result2(opus: (i64, f64), haiku: (i64, f64), main_tok: i64) -> String {
        let total = opus.1 + haiku.1;
        format!(
            r#"{{"type":"result","total_cost_usd":{total},"num_turns":3,"usage":{{"input_tokens":{main_tok}}},"modelUsage":{{"opus":{{"cacheReadInputTokens":{},"costUSD":{}}},"haiku":{{"inputTokens":{},"costUSD":{}}}}}}}"#,
            opus.0, opus.1, haiku.0, haiku.1
        )
    }

    #[tokio::test]
    async fn rebuild_splits_models_and_keeps_timestamps() {
        let db = db().await;
        legacy_row(&db, "R", "2026-10-10T00:00:00+00:00", 9.9).await;
        legacy_row(&db, "R", "2026-10-11T00:00:00+00:00", 9.9).await;
        let log = [init("2.1.288"), result2((100, 1.0), (50, 0.1), 5), init("2.1.288"), result2((180, 1.8), (50, 0.1), 5)]
            .join("\n");
        let (turns, base) = turns_from_log(&log);
        assert!(rebuild_run(&db, "R", &turns, base.as_ref()).await.unwrap());
        let got = rows(&db, "R").await;
        assert_eq!(got.len(), 3);
        // Turn 1: both models; turn 2: only opus grew (haiku unchanged → no row).
        assert_eq!((got[0].0.as_str(), got[0].1.as_str(), got[0].2), ("2026-10-10T00:00:00+00:00", "haiku", 50));
        assert_eq!((got[1].1.as_str(), got[1].2), ("opus", 100));
        assert_eq!((got[2].0.as_str(), got[2].1.as_str(), got[2].2), ("2026-10-11T00:00:00+00:00", "opus", 80));
        assert!((got[2].3 - 0.8).abs() < 1e-9);
        // num_turns lands once per turn, on the costliest model's row.
        let nt: Vec<Option<i64>> = sqlx::query_scalar("SELECT num_turns FROM run_usage WHERE run_id = 'R' ORDER BY rowid")
            .fetch_all(&db)
            .await
            .unwrap();
        assert_eq!(nt, vec![None, Some(3), Some(3)]);
        // Rebuilt runs have a baseline and are skipped next time.
        assert!(load_baseline(&db, "R").await.is_some());
        assert!(!rebuild_run(&db, "R", &turns, base.as_ref()).await.unwrap());
    }

    #[tokio::test]
    async fn rebuild_skips_misaligned_runs() {
        let db = db().await;
        legacy_row(&db, "R", "2026-10-10T00:00:00+00:00", 1.0).await;
        let log = [init("2.1.288"), result(10, 0.1, 5), init("2.1.288"), result(20, 0.2, 5)].join("\n");
        let (turns, base) = turns_from_log(&log);
        assert!(!rebuild_run(&db, "R", &turns, base.as_ref()).await.unwrap());
        assert_eq!(rows(&db, "R").await.len(), 1);
    }

    #[tokio::test]
    async fn live_turns_difference_against_baseline() {
        let db = db().await;
        let meta = RowMeta {
            run_id: "L",
            project_id: "p",
            project_name: "proj",
            engine: "claude",
            created_at: "2026-10-17T00:00:00+00:00",
            deleted_run: false,
        };
        let v = serde_json::json!({});
        // First turn of a fresh run: the whole total.
        let cur1 = mu(&[("opus", 100, 1.0)]);
        let pm = live_turn_usage(&db, "L", &cur1, Some("2.1.288"), 0).await;
        assert_eq!(pm, Some(cur1.clone()));
        let b1 = Baseline { usage: cur1, carries_over: true };
        commit_live_turn(&db, &meta, pm.as_ref(), &v, &b1).await.unwrap();
        // Resumed turn: difference, subagent model included.
        let cur2 = mu(&[("opus", 130, 1.3), ("haiku", 20, 0.05)]);
        let pm = live_turn_usage(&db, "L", &cur2, Some("2.1.288"), 0).await.unwrap();
        assert_eq!(pm["opus"].cache_read, 30);
        assert_eq!(pm["haiku"].cache_read, 20);
    }

    #[tokio::test]
    async fn live_turn_without_baseline_defers_to_legacy() {
        let db = db().await;
        legacy_row(&db, "X", "2026-10-01T00:00:00+00:00", 1.0).await;
        assert_eq!(live_turn_usage(&db, "X", &mu(&[("opus", 500, 5.0)]), Some("2.1.288"), 0).await, None);
    }
}
