//! Token/cost usage statistics over the independent `run_usage` ledger.
//!
//! `get_usage_stats` aggregates the ledger (with optional date/engine/project
//! filters) into a summary, a daily timeseries, and breakdowns by engine,
//! project, and model. `backfill_usage` reconstructs the ledger from existing
//! run logs. `reset_usage_stats` wipes all runs and the ledger to start over.

use crate::commands::runs::delete_run_inner;
use crate::db::Db;
use crate::runs::sidecar::{
    augment_command_path, detach_process_group, insert_usage_from_result,
    persist_plan_init_rate_limits, persist_plan_usage, persist_plan_rate_limit_event,
    resolve_sidecar,
};
use crate::runs::RunRegistry;
use chrono::{Datelike, Duration, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Row, Sqlite};
use std::collections::{HashMap, HashSet};
use tauri::{AppHandle, Emitter, State};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time;

#[derive(Debug, Default, Deserialize)]
pub struct StatsFilter {
    /// Inclusive lower bound, `YYYY-MM-DD`.
    pub from: Option<String>,
    /// Inclusive upper bound, `YYYY-MM-DD`.
    pub to: Option<String>,
    pub engine: Option<String>,
    pub project_id: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct UsageSummary {
    pub total_tokens: i64,
    pub total_input: i64,
    pub total_output: i64,
    pub total_cache: i64,
    pub total_cost: f64,
    pub estimated_cost: f64,
    pub total_runs: i64,
    pub total_turns: i64,
}

#[derive(Debug, Serialize)]
pub struct DailyPoint {
    pub date: String,
    pub tokens: i64,
    pub cost: f64,
    pub runs: i64,
}

#[derive(Debug, Serialize)]
pub struct EngineStat {
    pub engine: String,
    pub tokens: i64,
    pub cost: f64,
    pub runs: i64,
}

#[derive(Debug, Serialize)]
pub struct ProjectStat {
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub tokens: i64,
    pub cost: f64,
    pub runs: i64,
}

#[derive(Debug, Serialize)]
pub struct ModelStat {
    pub model: Option<String>,
    pub tokens: i64,
    pub cost: f64,
    pub runs: i64,
}

#[derive(Debug, Serialize)]
pub struct StatsResult {
    pub summary: UsageSummary,
    pub daily: Vec<DailyPoint>,
    pub by_engine: Vec<EngineStat>,
    pub by_project: Vec<ProjectStat>,
    pub by_model: Vec<ModelStat>,
}

/// One accumulator bucket. `runs` tracks distinct run ids so multi-turn runs
/// aren't counted more than once.
#[derive(Default)]
struct Bucket {
    tokens: i64,
    cost: f64,
    runs: HashSet<String>,
}

impl Bucket {
    fn add(&mut self, tokens: i64, cost: f64, run_key: &str) {
        self.tokens += tokens;
        self.cost += cost;
        self.runs.insert(run_key.to_string());
    }
}

#[tauri::command]
pub async fn get_usage_stats(
    db: State<'_, Db>,
    filter: StatsFilter,
) -> Result<StatsResult, String> {
    let mut qb = QueryBuilder::<Sqlite>::new(
        "SELECT run_id, project_id, project_name, engine, model, \
         input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, \
         total_tokens, cost_usd, cost_estimated, num_turns, created_at \
         FROM run_usage WHERE 1 = 1",
    );
    if let Some(f) = filter.from.as_ref().filter(|s| !s.is_empty()) {
        qb.push(" AND substr(created_at, 1, 10) >= ").push_bind(f.clone());
    }
    if let Some(t) = filter.to.as_ref().filter(|s| !s.is_empty()) {
        qb.push(" AND substr(created_at, 1, 10) <= ").push_bind(t.clone());
    }
    if let Some(e) = filter.engine.as_ref().filter(|s| !s.is_empty()) {
        qb.push(" AND engine = ").push_bind(e.clone());
    }
    if let Some(p) = filter.project_id.as_ref().filter(|s| !s.is_empty()) {
        qb.push(" AND project_id = ").push_bind(p.clone());
    }

    let rows = qb
        .build()
        .fetch_all(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    let mut summary = UsageSummary::default();
    let mut all_runs: HashSet<String> = HashSet::new();
    let mut daily: HashMap<String, Bucket> = HashMap::new();
    let mut by_engine: HashMap<String, Bucket> = HashMap::new();
    let mut by_project: HashMap<String, (Option<String>, Bucket)> = HashMap::new();
    let mut by_model: HashMap<String, Bucket> = HashMap::new();

    for (i, row) in rows.iter().enumerate() {
        let run_id: Option<String> = row.get("run_id");
        let run_key = run_id.unwrap_or_else(|| format!("__row_{i}"));
        let project_id: Option<String> = row.get("project_id");
        let project_name: Option<String> = row.get("project_name");
        let engine: String = row.get("engine");
        let model: Option<String> = row.get("model");
        let input: i64 = row.get("input_tokens");
        let output: i64 = row.get("output_tokens");
        let cache_creation: i64 = row.get("cache_creation_tokens");
        let cache_read: i64 = row.get("cache_read_tokens");
        let total: i64 = row.get("total_tokens");
        let cost: f64 = row.try_get::<f64, _>("cost_usd").unwrap_or(0.0);
        let estimated: i64 = row.get("cost_estimated");
        // SDK-reported turns for this result event; fall back to 1 when absent
        // (legacy rows, or engines that don't report it) so every result still
        // counts as at least one turn.
        let num_turns: i64 = row
            .try_get::<Option<i64>, _>("num_turns")
            .ok()
            .flatten()
            .filter(|n| *n > 0)
            .unwrap_or(1);
        let created_at: String = row.get("created_at");
        let date = created_at.chars().take(10).collect::<String>();

        summary.total_tokens += total;
        summary.total_input += input;
        summary.total_output += output;
        summary.total_cache += cache_creation + cache_read;
        summary.total_cost += cost;
        if estimated != 0 {
            summary.estimated_cost += cost;
        }
        summary.total_turns += num_turns;
        all_runs.insert(run_key.clone());

        daily.entry(date).or_default().add(total, cost, &run_key);
        by_engine.entry(engine).or_default().add(total, cost, &run_key);
        let proj_key = project_id.clone().unwrap_or_else(|| "__none".to_string());
        let proj_entry = by_project
            .entry(proj_key)
            .or_insert_with(|| (project_name.clone(), Bucket::default()));
        proj_entry.1.add(total, cost, &run_key);
        let model_key = model.clone().unwrap_or_else(|| "__unknown".to_string());
        by_model.entry(model_key).or_default().add(total, cost, &run_key);
    }

    summary.total_runs = all_runs.len() as i64;

    let mut daily: Vec<DailyPoint> = daily
        .into_iter()
        .map(|(date, b)| DailyPoint {
            date,
            tokens: b.tokens,
            cost: b.cost,
            runs: b.runs.len() as i64,
        })
        .collect();
    daily.sort_by(|a, b| a.date.cmp(&b.date));

    let mut by_engine: Vec<EngineStat> = by_engine
        .into_iter()
        .map(|(engine, b)| EngineStat {
            engine,
            tokens: b.tokens,
            cost: b.cost,
            runs: b.runs.len() as i64,
        })
        .collect();
    by_engine.sort_by(|a, b| b.tokens.cmp(&a.tokens));

    let mut by_project: Vec<ProjectStat> = by_project
        .into_iter()
        .map(|(pid, (name, b))| ProjectStat {
            project_id: if pid == "__none" { None } else { Some(pid) },
            project_name: name,
            tokens: b.tokens,
            cost: b.cost,
            runs: b.runs.len() as i64,
        })
        .collect();
    by_project.sort_by(|a, b| b.tokens.cmp(&a.tokens));

    let mut by_model: Vec<ModelStat> = by_model
        .into_iter()
        .map(|(model, b)| ModelStat {
            model: if model == "__unknown" { None } else { Some(model) },
            tokens: b.tokens,
            cost: b.cost,
            runs: b.runs.len() as i64,
        })
        .collect();
    by_model.sort_by(|a, b| b.tokens.cmp(&a.tokens));

    Ok(StatsResult {
        summary,
        daily,
        by_engine,
        by_project,
        by_model,
    })
}

#[derive(Debug, Serialize)]
pub struct BackfillResult {
    pub inserted: usize,
    pub runs_scanned: usize,
}

/// Reconstruct the usage ledger from existing run logs. Idempotent: runs that
/// already have ledger rows are skipped, so live-captured data isn't disturbed.
#[tauri::command]
pub async fn backfill_usage(db: State<'_, Db>) -> Result<BackfillResult, String> {
    let runs = sqlx::query(
        "SELECT r.id, r.project_id, r.engine, r.output_path, r.finished_at, p.name AS project_name \
         FROM runs r JOIN projects p ON p.id = r.project_id \
         WHERE r.output_path IS NOT NULL",
    )
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    let mut inserted = 0usize;
    let mut runs_scanned = 0usize;

    for row in &runs {
        let run_id: String = row.get("id");
        let existing: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM run_usage WHERE run_id = ?")
                .bind(&run_id)
                .fetch_one(db.inner())
                .await
                .unwrap_or(0);
        if existing > 0 {
            continue;
        }

        let output_path: String = row.get("output_path");
        let Ok(content) = std::fs::read_to_string(&output_path) else {
            continue;
        };
        runs_scanned += 1;

        let project_id: String = row.get("project_id");
        let project_name: String = row.get("project_name");
        let engine: String = row.get("engine");
        let finished_at: Option<String> = row.get("finished_at");
        let created_at = finished_at.unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

        let mut last_model: Option<String> = None;
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('[') {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            match v.get("type").and_then(|x| x.as_str()) {
                Some("system")
                    if v.get("subtype").and_then(|x| x.as_str()) == Some("init") =>
                {
                    if let Some(m) = v.get("model").and_then(|x| x.as_str()) {
                        last_model = Some(m.to_string());
                    }
                }
                Some("result") => {
                    if insert_usage_from_result(
                        db.inner(),
                        &run_id,
                        &project_id,
                        &project_name,
                        &engine,
                        last_model.as_deref(),
                        &v,
                        &created_at,
                    )
                    .await
                    {
                        inserted += 1;
                    }
                }
                _ => {}
            }
        }
    }

    Ok(BackfillResult {
        inserted,
        runs_scanned,
    })
}

#[derive(Debug, Serialize)]
pub struct ResetResult {
    pub runs_deleted: u32,
    pub usage_cleared: u64,
}

/// Wipe ALL runs (and their log files) and the entire usage ledger so counting
/// starts from zero. The only path that clears `run_usage`. Refuses individual
/// runs that are still active (those are simply skipped).
#[tauri::command]
pub async fn reset_usage_stats(
    db: State<'_, Db>,
    registry: State<'_, RunRegistry>,
) -> Result<ResetResult, String> {
    let rows = sqlx::query("SELECT id FROM runs WHERE status != 'running'")
        .fetch_all(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    let mut runs_deleted = 0u32;
    for row in &rows {
        let id: String = row.get("id");
        if delete_run_inner(db.inner(), registry.inner(), &id)
            .await
            .is_ok()
        {
            runs_deleted += 1;
        }
    }

    let res = sqlx::query("DELETE FROM run_usage")
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    Ok(ResetResult {
        runs_deleted,
        usage_cleared: res.rows_affected(),
    })
}

// ── Global token budget ───────────────────────────────────────────────────
//
// A guardrail over total token consumption across ALL runs (unrelated to the
// per-run context-window meter). Periods are computed in UTC to match the
// RFC3339 UTC timestamps stored in `run_usage.created_at`. "5h" is a rolling
// window (matching Claude's rolling session limit); "week"/"month" reset on a
// fixed boundary (Monday / first of month).

/// Inclusive lower bound of the current budget period.
pub fn period_start(period: &str) -> chrono::DateTime<Utc> {
    let now = Utc::now();
    match period {
        "5h" => now - Duration::hours(5),
        "week" => {
            let days = now.weekday().num_days_from_monday() as i64;
            let monday = (now - Duration::days(days)).date_naive();
            Utc.with_ymd_and_hms(monday.year(), monday.month(), monday.day(), 0, 0, 0)
                .unwrap()
        }
        _ => Utc
            .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
            .unwrap(),
    }
}

/// When the current period resets, or None for the rolling "5h" window.
pub fn next_reset(period: &str) -> Option<chrono::DateTime<Utc>> {
    let now = Utc::now();
    match period {
        "week" => Some(period_start("week") + Duration::days(7)),
        "month" => {
            let (y, m) = if now.month() == 12 {
                (now.year() + 1, 1)
            } else {
                (now.year(), now.month() + 1)
            };
            Some(Utc.with_ymd_and_hms(y, m, 1, 0, 0, 0).unwrap())
        }
        _ => None,
    }
}

/// Sum of `total_tokens` across all runs since `since` (RFC3339).
pub async fn token_usage_since(pool: &Db, since: &str) -> Result<i64, String> {
    let total: i64 =
        sqlx::query_scalar("SELECT COALESCE(SUM(total_tokens), 0) FROM run_usage WHERE created_at >= ?")
            .bind(since)
            .fetch_one(pool)
            .await
            .map_err(|e| e.to_string())?;
    Ok(total)
}

/// The single budget verdict shared by the backend guardrail and the frontend
/// badge. Prefers real subscription plan utilization (`/usage`); falls back to
/// the self-imposed token budget; otherwise disabled. Computed in one place
/// (`budget_status`) so the badge and the run-blocking logic can never diverge.
#[derive(Debug, Serialize)]
pub struct BudgetStatus {
    /// "plan" (real /usage window) | "tokens" (self-imposed) | "disabled".
    pub source: String,
    pub period: String,
    /// 0-100+; percent of the effective limit currently consumed.
    pub percent: i64,
    pub is_warning: bool,
    pub is_over: bool,
    /// Local token tally for the period (only meaningful when source = tokens).
    pub used_tokens: i64,
    pub limit_tokens: i64,
    /// RFC3339 reset instant, or null for a rolling window with no fixed reset.
    pub reset: Option<String>,
    /// RFC3339 time the plan snapshot was captured, if source = plan.
    pub captured_at: Option<String>,
    /// True when the displayed plan snapshot is older than the live-refresh
    /// window. UI should label it as cached instead of live.
    pub is_stale: bool,
    /// Live severity for the active plan window: allowed|warning|blocked, from
    /// the newest rate_limit_event. None for token/disabled sources.
    #[serde(default)]
    pub status: Option<String>,
    /// True when the active window's reset time has passed since the % was
    /// captured, so the displayed percent is a stale pre-reset value.
    #[serde(default)]
    pub rolled_over: bool,
}

// ── Plan (subscription) usage — real /usage data ───────────────────────────
//
// The structured data behind Claude Code's `/usage` command: claude.ai plan
// rate-limit utilization (0-100%) and the real per-account reset time for each
// window. Captured opportunistically from the live Claude session by the
// sidecar (see `persist_plan_usage`) and stored as the latest-known snapshot in
// the `settings` KV under `plan_usage`. Unlike the self-imposed token budget,
// these are the account's actual subscription limits.

#[derive(Debug, Serialize, Deserialize)]
pub struct PlanWindow {
    /// Percentage of the window used, 0-100. None when the window is absent.
    pub utilization: Option<f64>,
    /// ISO 8601 instant the window resets. None for non-subscription sessions.
    pub resets_at: Option<String>,
    /// Live severity from the latest `rate_limit_event`: allowed|warning|blocked.
    #[serde(default)]
    pub status: Option<String>,
    /// RFC3339 time `status` was last observed (independent of `utilization`).
    #[serde(default)]
    pub status_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PlanWindows {
    pub five_hour: PlanWindow,
    pub seven_day: PlanWindow,
    pub seven_day_opus: PlanWindow,
    pub seven_day_sonnet: PlanWindow,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PlanUsage {
    /// RFC3339 time this snapshot was captured from a live session.
    pub captured_at: String,
    /// 'pro' | 'max' | 'team' | 'enterprise', or null for API-key sessions.
    pub subscription_type: Option<String>,
    /// False for API key / 3P-provider sessions where plan limits don't apply.
    pub rate_limits_available: bool,
    pub windows: PlanWindows,
}

/// Latest known subscription plan-usage snapshot, or None if none captured yet
/// (no Claude run has executed since install, or the user is on API billing).
#[tauri::command]
pub async fn get_plan_usage(db: State<'_, Db>) -> Result<Option<PlanUsage>, String> {
    load_plan_usage(db.inner()).await
}

async fn load_plan_usage(db: &Db) -> Result<Option<PlanUsage>, String> {
    let stored: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'plan_usage'")
            .fetch_optional(db)
            .await
            .map_err(|e| e.to_string())?;
    match stored {
        Some(json) => serde_json::from_str(&json).map(Some).map_err(|e| e.to_string()),
        None => Ok(None),
    }
}

#[tauri::command]
pub async fn refresh_plan_usage(
    app: AppHandle,
    db: State<'_, Db>,
) -> Result<Option<PlanUsage>, String> {
    let rows = sqlx::query("SELECT key, value FROM settings")
        .fetch_all(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    let mut node_path = "node".to_string();
    let mut sidecar_path = String::new();
    let mut claude_path = "claude".to_string();
    for row in &rows {
        let key: String = row.get("key");
        let value: String = row.get("value");
        match key.as_str() {
            "node_path" => node_path = value,
            "sidecar_path" => sidecar_path = value,
            "claude_path" => claude_path = value,
            _ => {}
        }
    }

    let cwd: String = sqlx::query_scalar("SELECT path FROM projects ORDER BY created_at DESC LIMIT 1")
        .fetch_optional(db.inner())
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| ".".to_string())
        });

    let (node_bin, sidecar_script) = resolve_sidecar(&app, &node_path, &sidecar_path)?;
    let mut cmd = Command::new(&node_bin);
    cmd.current_dir(&cwd).arg(&sidecar_script);
    augment_command_path(&mut cmd);
    if claude_path != "claude" && !claude_path.trim().is_empty() {
        cmd.env("DEVDY_CLAUDE_PATH", &claude_path);
    }
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    detach_process_group(&mut cmd);
    cmd.kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn usage probe ({}): {}", node_bin, e))?;
    let mut stdin = child.stdin.take().ok_or("usage probe stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("usage probe stdout unavailable")?;
    let mut lines = BufReader::new(stdout).lines();

    let probe = serde_json::json!({
        "type": "usage_probe",
        "options": { "cwd": cwd },
    });
    stdin
        .write_all(format!("{}\n", probe).as_bytes())
        .await
        .map_err(|e| format!("write usage probe: {}", e))?;
    stdin.flush().await.map_err(|e| format!("flush usage probe: {}", e))?;

    let db_pool = db.inner().clone();
    let read_usage = async {
        while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            match value.get("type").and_then(|v| v.as_str()) {
                Some("_devdy_usage") => {
                    if let Some(usage) = value.get("usage") {
                        if usage
                            .get("rate_limits_available")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                            && usage.get("rate_limits").and_then(|v| v.as_object()).is_some()
                        {
                            if persist_plan_usage(&db_pool, usage).await {
                                let _ = app.emit("plan_usage_updated", serde_json::json!({ "source": "probe" }));
                                return Ok(true);
                            }
                        }
                    }
                }
                Some("system") => {
                    if persist_plan_init_rate_limits(&db_pool, &value).await {
                        let _ = app.emit("plan_usage_updated", serde_json::json!({ "source": "probe" }));
                        return Ok(true);
                    }
                }
                Some("rate_limit_event") => {
                    if persist_plan_rate_limit_event(&db_pool, &value).await {
                        let _ = app.emit("plan_usage_updated", serde_json::json!({ "source": "probe" }));
                        return Ok(true);
                    }
                }
                Some("_devdy_done") | Some("_devdy_closed") => return Ok(false),
                Some("_devdy_error") => {
                    let err = value
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("usage probe failed");
                    return Err(err.to_string());
                }
                _ => {}
            }
        }
        Ok(false)
    };

    let outcome = time::timeout(std::time::Duration::from_secs(15), read_usage).await;
    if let Some(pid) = child.id() {
        crate::runs::sidecar::kill_process_group(pid);
    }
    let _ = child.kill().await;

    match outcome {
        Ok(Ok(true)) => load_plan_usage(db.inner()).await,
        Ok(Ok(false)) => Err("usage probe completed without a fresh usage snapshot".to_string()),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("usage probe timed out".to_string()),
    }
}

/// Map the configured budget period onto a real claude.ai plan window from the
/// latest `/usage` snapshot. Returns `(utilization_percent, resets_at)` only
/// when accurate plan data backs the period: rolling 5h → five_hour, weekly →
/// seven_day. "Monthly" has no plan window, so it always falls through to the
/// token estimate. Mirrors the mapping in `src/stores/budget.ts`.
struct PlanWindowView {
    utilization: f64,
    resets_at: Option<String>,
    captured_at: Option<String>,
    is_stale: bool,
    status: Option<String>,
    rolled_over: bool,
}

fn plan_window_for(period: &str, snapshot: &serde_json::Value, stale_secs: i64) -> Option<PlanWindowView> {
    if !snapshot.get("rate_limits_available").and_then(|v| v.as_bool()).unwrap_or(false) {
        return None;
    }
    let key = match period {
        "5h" => "five_hour",
        "week" => "seven_day",
        _ => return None,
    };
    let w = snapshot.get("windows").and_then(|w| w.get(key))?;
    let resets_at = w.get("resets_at").and_then(|v| v.as_str()).map(|s| s.to_string());
    let status = w.get("status").and_then(|v| v.as_str()).map(|s| s.to_string());
    let captured_at = snapshot
        .get("captured_at")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // The window has rolled over when its reset instant is already in the past:
    // the stored % predates the reset and no longer reflects the fresh window.
    let now = Utc::now();
    let rolled_over = resets_at
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| now > dt.with_timezone(&Utc))
        .unwrap_or(false);

    // Utilization only comes from `/usage`. Missing % is fine after a rollover
    // (treat as 0); otherwise there's no plan verdict yet — fall through.
    let util = match w.get("utilization").and_then(|v| v.as_f64()) {
        Some(u) if !rolled_over => u,
        _ if rolled_over => 0.0,
        _ => return None,
    };

    let is_stale = captured_at
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| now.signed_duration_since(dt.with_timezone(&Utc)) > Duration::seconds(stale_secs.max(1)))
        .unwrap_or(true);
    Some(PlanWindowView {
        utilization: util,
        resets_at,
        captured_at,
        is_stale,
        status,
        rolled_over,
    })
}

/// The one place that decides whether usage is near/over budget. Reads the
/// configured period / token limit / warn threshold from settings, the latest
/// plan-usage snapshot, and the period's token tally — then prefers real plan
/// utilization over the self-imposed token estimate. Used by both the
/// `get_budget_status` command (badge) and `enforce_budget` (run blocking).
pub async fn budget_status(db: &Db) -> Result<BudgetStatus, String> {
    let rows = sqlx::query("SELECT key, value FROM settings")
        .fetch_all(db)
        .await
        .map_err(|e| e.to_string())?;
    let mut period = "month".to_string();
    let mut limit_tokens: i64 = 0;
    let mut warn_percent: i64 = 80;
    let mut stale_secs: i64 = 120;
    let mut plan_json: Option<String> = None;
    for row in &rows {
        let key: String = row.get("key");
        let value: String = row.get("value");
        match key.as_str() {
            "token_budget_period" => period = value,
            "token_budget_limit" => limit_tokens = value.trim().parse::<i64>().unwrap_or(0).max(0),
            "budget_warn_percent" => warn_percent = value.trim().parse::<i64>().unwrap_or(80).clamp(1, 100),
            "plan_stale_secs" => stale_secs = value.trim().parse::<i64>().unwrap_or(120).clamp(10, 3600),
            "plan_usage" => plan_json = Some(value),
            _ => {}
        }
    }

    let reset = next_reset(&period).map(|d| d.to_rfc3339());

    // 1) Real subscription plan window (most accurate — the true ceiling).
    if let Some(snapshot) = plan_json.as_deref().and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok()) {
        if let Some(view) = plan_window_for(&period, &snapshot, stale_secs) {
            let percent = view.utilization.round() as i64;
            let blocked = view.status.as_deref() == Some("blocked");
            let warning_status = view.status.as_deref() == Some("warning");
            return Ok(BudgetStatus {
                source: "plan".into(),
                period,
                percent,
                // Live status escalates the verdict even when the % is slightly
                // stale; a rolled-over window can never be warning/over.
                is_warning: !view.rolled_over && (percent >= warn_percent || warning_status || blocked),
                is_over: !view.rolled_over && (percent >= 100 || blocked),
                used_tokens: 0,
                limit_tokens: 0,
                reset: view.resets_at,
                captured_at: view.captured_at,
                is_stale: view.is_stale,
                status: view.status,
                rolled_over: view.rolled_over,
            });
        }
    }

    // 2) Self-imposed token budget (fallback: Codex, monthly, API sessions).
    if limit_tokens > 0 {
        let start = period_start(&period).to_rfc3339();
        let used_tokens = token_usage_since(db, &start).await?;
        let percent = ((used_tokens as f64 / limit_tokens as f64) * 100.0).round() as i64;
        return Ok(BudgetStatus {
            source: "tokens".into(),
            period,
            percent,
            is_warning: percent >= warn_percent,
            is_over: used_tokens >= limit_tokens,
            used_tokens,
            limit_tokens,
            reset,
            captured_at: None,
            is_stale: false,
            status: None,
            rolled_over: false,
        });
    }

    // 3) No guardrail configured and no plan data — disabled.
    Ok(BudgetStatus {
        source: "disabled".into(),
        period,
        percent: 0,
        is_warning: false,
        is_over: false,
        used_tokens: 0,
        limit_tokens: 0,
        reset,
        captured_at: None,
        is_stale: false,
        status: None,
        rolled_over: false,
    })
}

#[tauri::command]
pub async fn get_budget_status(db: State<'_, Db>) -> Result<BudgetStatus, String> {
    budget_status(db.inner()).await
}

/// Guardrail used at every token-consuming entry point (start / resume /
/// follow-up). Refuses when the budget is over, unless the user explicitly
/// overrode it. The error is prefixed `BUDGET_EXCEEDED` so the UI can offer a
/// one-click override.
pub async fn enforce_budget(db: &Db, override_budget: bool) -> Result<(), String> {
    if override_budget {
        return Ok(());
    }
    let status = budget_status(db).await?;
    if status.is_over {
        return Err(format!(
            "BUDGET_EXCEEDED: {} usage at {}% of the {} limit",
            status.source, status.percent, status.period
        ));
    }
    Ok(())
}

#[cfg(test)]
mod plan_window_tests {
    use super::*;

    fn snapshot(five_hour: serde_json::Value, captured_at: &str) -> serde_json::Value {
        serde_json::json!({
            "captured_at": captured_at,
            "rate_limits_available": true,
            "windows": { "five_hour": five_hour },
        })
    }

    #[test]
    fn fresh_utilization_is_not_stale() {
        let now = Utc::now().to_rfc3339();
        let future = (Utc::now() + Duration::hours(2)).to_rfc3339();
        let snap = snapshot(
            serde_json::json!({ "utilization": 42.0, "resets_at": future, "status": "allowed" }),
            &now,
        );
        let v = plan_window_for("5h", &snap, 120).expect("plan window");
        assert_eq!(v.utilization.round() as i64, 42);
        assert!(!v.is_stale);
        assert!(!v.rolled_over);
        assert_eq!(v.status.as_deref(), Some("allowed"));
    }

    #[test]
    fn old_snapshot_is_stale() {
        let old = (Utc::now() - Duration::minutes(30)).to_rfc3339();
        let future = (Utc::now() + Duration::hours(2)).to_rfc3339();
        let snap = snapshot(
            serde_json::json!({ "utilization": 10.0, "resets_at": future }),
            &old,
        );
        let v = plan_window_for("5h", &snap, 120).expect("plan window");
        assert!(v.is_stale);
    }

    #[test]
    fn past_reset_rolls_over_to_zero() {
        let now = Utc::now().to_rfc3339();
        let past = (Utc::now() - Duration::minutes(5)).to_rfc3339();
        let snap = snapshot(
            serde_json::json!({ "utilization": 88.0, "resets_at": past }),
            &now,
        );
        let v = plan_window_for("5h", &snap, 120).expect("plan window");
        assert!(v.rolled_over);
        assert_eq!(v.utilization.round() as i64, 0);
    }

    #[test]
    fn missing_utilization_without_rollover_yields_no_verdict() {
        let now = Utc::now().to_rfc3339();
        let future = (Utc::now() + Duration::hours(2)).to_rfc3339();
        // Only a rate_limit_event landed (status/reset but no %): no plan verdict.
        let snap = snapshot(
            serde_json::json!({ "utilization": null, "resets_at": future, "status": "allowed" }),
            &now,
        );
        assert!(plan_window_for("5h", &snap, 120).is_none());
    }
}
