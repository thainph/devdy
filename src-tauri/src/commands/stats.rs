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
    resolve_codex_sidecar, resolve_sidecar,
};
use crate::runs::RunRegistry;
use chrono::{Duration, Utc};
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

// ── Run-blocking budget guardrail ──────────────────────────────────────────
//
// A guardrail over real subscription plan usage. Two windows mirror the actual
// Claude/Codex limits: "5h" (rolling session window) and "week" (weekly window,
// resets per account). Each has its own block-at-% threshold; a run is refused
// when its engine reaches any configured window. There is no token-based
// fallback — the guardrail applies only when real plan data is available.

/// A budget verdict: real subscription plan utilization (`/usage`) or disabled.
/// The badge display (`plan_display_status`) and the run-blocking guardrail
/// (`budget_status_for`) both produce this shape but from independent inputs.
#[derive(Debug, Serialize)]
pub struct BudgetStatus {
    /// "plan" (real /usage window) | "disabled".
    pub source: String,
    pub period: String,
    /// 0-100+; percent of the window's limit currently consumed.
    pub percent: i64,
    pub is_warning: bool,
    pub is_over: bool,
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
    load_plan_usage_key(db, "plan_usage").await
}

async fn load_plan_usage_key(db: &Db, key: &str) -> Result<Option<PlanUsage>, String> {
    let stored: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(db)
        .await
        .map_err(|e| e.to_string())?;
    match stored {
        Some(json) => serde_json::from_str(&json).map(Some).map_err(|e| e.to_string()),
        None => Ok(None),
    }
}

/// Latest Codex plan-usage snapshot (the `/status` rate-limits), or None if none
/// captured yet. Mirrors [`get_plan_usage`] for the Codex account.
#[tauri::command]
pub async fn get_plan_usage_codex(db: State<'_, Db>) -> Result<Option<PlanUsage>, String> {
    load_plan_usage_key(db.inner(), "plan_usage_codex").await
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

/// Probe Codex now for fresh plan rate-limits via the app-server
/// `account/rateLimits/read` RPC (no thread, no turn, no quota spend). Spawns the
/// Codex sidecar in probe mode, persists the returned snapshot to
/// `plan_usage_codex`, and returns it. Mirrors [`refresh_plan_usage`].
#[tauri::command]
pub async fn refresh_codex_plan_usage(
    app: AppHandle,
    db: State<'_, Db>,
) -> Result<Option<PlanUsage>, String> {
    let rows = sqlx::query("SELECT key, value FROM settings")
        .fetch_all(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    let mut node_path = "node".to_string();
    let mut codex_sidecar_path = String::new();
    let mut codex_path = "codex".to_string();
    for row in &rows {
        let key: String = row.get("key");
        let value: String = row.get("value");
        match key.as_str() {
            "node_path" => node_path = value,
            "codex_sidecar_path" => codex_sidecar_path = value,
            "codex_path" => codex_path = value,
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

    let (node_bin, sidecar_script) = resolve_codex_sidecar(&app, &node_path, &codex_sidecar_path)?;
    let mut cmd = Command::new(&node_bin);
    cmd.current_dir(&cwd).arg(&sidecar_script);
    augment_command_path(&mut cmd);
    cmd.env("DEVDY_CODEX_USAGE_PROBE", "1");
    if codex_path != "codex" && !codex_path.trim().is_empty() {
        cmd.env("DEVDY_CODEX_PATH", &codex_path);
    }
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    detach_process_group(&mut cmd);
    cmd.kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn codex usage probe ({}): {}", node_bin, e))?;
    let stdout = child.stdout.take().ok_or("codex usage probe stdout unavailable")?;
    let mut lines = BufReader::new(stdout).lines();

    let db_pool = db.inner().clone();
    let read_usage = async {
        while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            match value.get("type").and_then(|v| v.as_str()) {
                Some("_devdy_usage") => {
                    if let Some(rl) = value.get("usage").and_then(|u| u.get("rate_limits")) {
                        if crate::commands::codex_sessions::persist_codex_plan_usage(&db_pool, rl).await {
                            let _ = app.emit("plan_usage_updated", serde_json::json!({ "provider": "codex", "source": "probe" }));
                            return Ok(true);
                        }
                    }
                }
                Some("_devdy_done") | Some("_devdy_closed") => return Ok(false),
                Some("_devdy_error") => {
                    let err = value
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("codex usage probe failed");
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
        Ok(Ok(true)) => load_plan_usage_key(db.inner(), "plan_usage_codex").await,
        Ok(Ok(false)) => Err("codex usage probe completed without a fresh usage snapshot".to_string()),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("codex usage probe timed out".to_string()),
    }
}

/// A single plan window resolved from the latest `/usage` snapshot: its
/// utilization plus the freshness/rollover metadata needed by both the badge
/// display and the run-blocking guardrail.
struct PlanWindowView {
    utilization: f64,
    resets_at: Option<String>,
    captured_at: Option<String>,
    is_stale: bool,
    status: Option<String>,
    rolled_over: bool,
}

/// Plan-window key → the period label to show for it.
fn window_key_to_period(key: &str) -> &'static str {
    match key {
        "five_hour" => "5h",
        _ => "week", // seven_day / seven_day_opus / seven_day_sonnet are weekly
    }
}

/// Highest-utilization weekly window (all-models `seven_day` plus the per-model
/// weekly windows when present) — the binding weekly constraint for the guardrail.
fn weekly_window(snapshot: &serde_json::Value, stale_secs: i64) -> Option<PlanWindowView> {
    let mut best: Option<PlanWindowView> = None;
    for k in ["seven_day", "seven_day_opus", "seven_day_sonnet"] {
        if let Some(v) = plan_window_by_key(k, snapshot, stale_secs) {
            let better = best.as_ref().map(|b| v.utilization > b.utilization).unwrap_or(true);
            if better {
                best = Some(v);
            }
        }
    }
    best
}

fn plan_window_by_key(key: &str, snapshot: &serde_json::Value, stale_secs: i64) -> Option<PlanWindowView> {
    if !snapshot.get("rate_limits_available").and_then(|v| v.as_bool()).unwrap_or(false) {
        return None;
    }
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

/// Display threshold for the badge's warning tone, deliberately decoupled from
/// the guardrail's block thresholds (`budget_5h_percent` / `budget_week_percent`).
/// The badge reflects the account's REAL plan usage; the configured Usage budget
/// only gates run-blocking (see `enforce_budget`). 80% is a fixed, sensible
/// "getting close" heuristic.
const PLAN_DISPLAY_WARN_PERCENT: i64 = 80;

/// Read the latest plan snapshot JSON (`key`) and `plan_stale_secs` from the
/// settings KV — everything the badge needs, without touching the guardrail's
/// block-threshold / token-limit settings.
async fn read_plan_snapshot(db: &Db, key: &str) -> Result<(Option<String>, i64), String> {
    let rows = sqlx::query("SELECT key, value FROM settings")
        .fetch_all(db)
        .await
        .map_err(|e| e.to_string())?;
    let mut plan_json: Option<String> = None;
    let mut stale_secs: i64 = 120;
    for row in &rows {
        let k: String = row.get("key");
        let value: String = row.get("value");
        match k.as_str() {
            s if s == key => plan_json = Some(value),
            "plan_stale_secs" => {
                stale_secs = value.trim().parse::<i64>().unwrap_or(120).clamp(10, 3600)
            }
            _ => {}
        }
    }
    Ok((plan_json, stale_secs))
}

/// Pick the SMALLEST available plan window, independent of any configured budget
/// period. Preference order: rolling 5h first, then the weekly windows. So the
/// badge always shows the 5h window when its data is present, and only falls
/// back to weekly when it isn't. This is a real plan-usage glance, not the
/// guardrail.
fn plan_display_view(
    snapshot: &serde_json::Value,
    stale_secs: i64,
) -> Option<(String, PlanWindowView)> {
    for k in ["five_hour", "seven_day", "seven_day_opus", "seven_day_sonnet"] {
        if let Some(v) = plan_window_by_key(k, snapshot, stale_secs) {
            return Some((window_key_to_period(k).to_string(), v));
        }
    }
    None
}

/// Build the badge verdict from a plan snapshot, independent of the Usage budget
/// setting. Warning/over come from the plan's own live status plus a fixed
/// display threshold — never from the guardrail block thresholds. Returns
/// `disabled` when no plan window is present (e.g. API-key sessions).
fn plan_display_status(plan_json: Option<&str>, stale_secs: i64) -> BudgetStatus {
    if let Some(snapshot) =
        plan_json.and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok())
    {
        if let Some((period, view)) = plan_display_view(&snapshot, stale_secs) {
            let percent = view.utilization.round() as i64;
            let blocked = view.status.as_deref() == Some("blocked");
            return BudgetStatus {
                source: "plan".into(),
                period,
                percent,
                // Amber is driven purely by the DISPLAYED window's own %. The live
                // `warning` status is deliberately NOT used here: Claude attaches
                // an "allowed_warning" state to a window even when that window's %
                // is low (the warning is about a different constraint), which made
                // the badge turn amber at ~18%. A real hard block still shows red.
                is_warning: !view.rolled_over && percent >= PLAN_DISPLAY_WARN_PERCENT,
                is_over: !view.rolled_over && (percent >= 100 || blocked),
                reset: view.resets_at,
                captured_at: view.captured_at,
                is_stale: view.is_stale,
                status: view.status,
                rolled_over: view.rolled_over,
            };
        }
    }
    BudgetStatus {
        source: "disabled".into(),
        period: "week".into(),
        percent: 0,
        is_warning: false,
        is_over: false,
        reset: None,
        captured_at: None,
        is_stale: false,
        status: None,
        rolled_over: false,
    }
}

/// Warning margin: a window is "approaching" its block threshold once it reaches
/// this fraction of it. Drives the more-careful Claude usage-capture mode.
const GUARDRAIL_WARN_RATIO: f64 = 0.9;

/// The run-blocking guardrail verdict for a specific `engine`. Checks that
/// engine's REAL plan windows — the rolling 5h window and the weekly window —
/// against the separately-configured block thresholds (`budget_5h_percent` /
/// `budget_week_percent`). Blocks when ANY configured window reaches its
/// threshold; a window with no threshold (empty) or no data is skipped. When the
/// engine has no plan data (e.g. API-key sessions) the guardrail is inactive.
/// Used by `enforce_budget` (run blocking), the Claude usage-capture mode, and
/// the composer lock.
pub async fn budget_status_for(db: &Db, engine: &str) -> Result<BudgetStatus, String> {
    let rows = sqlx::query("SELECT key, value FROM settings")
        .fetch_all(db)
        .await
        .map_err(|e| e.to_string())?;
    let mut block_5h: i64 = 0;
    let mut block_week: i64 = 0;
    let mut stale_secs: i64 = 120;
    let mut plan_claude: Option<String> = None;
    let mut plan_codex: Option<String> = None;
    for row in &rows {
        let key: String = row.get("key");
        let value: String = row.get("value");
        match key.as_str() {
            "budget_5h_percent" => block_5h = value.trim().parse::<i64>().unwrap_or(0).clamp(0, 100),
            "budget_week_percent" => block_week = value.trim().parse::<i64>().unwrap_or(0).clamp(0, 100),
            "plan_stale_secs" => stale_secs = value.trim().parse::<i64>().unwrap_or(120).clamp(10, 3600),
            "plan_usage" => plan_claude = Some(value),
            "plan_usage_codex" => plan_codex = Some(value),
            _ => {}
        }
    }

    let plan_json = if engine == "codex" { plan_codex } else { plan_claude };

    // 1) Real plan windows for this engine (the true ceiling).
    if let Some(snapshot) = plan_json
        .as_deref()
        .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok())
    {
        let five = plan_window_by_key("five_hour", &snapshot, stale_secs);
        let week = weekly_window(&snapshot, stale_secs);
        if five.is_some() || week.is_some() {
            let mut is_over = false;
            let mut is_warning = false;
            // Report the window nearest to (or furthest over) its threshold; a
            // configured window always outranks an unconfigured one.
            let mut report: Option<(i64, &'static str, PlanWindowView)> = None;
            for (view_opt, threshold, period) in [(five, block_5h, "5h"), (week, block_week, "week")] {
                let Some(view) = view_opt else { continue };
                let util = view.utilization.round() as i64;
                let blocked = view.status.as_deref() == Some("blocked");
                let warning_status = view.status.as_deref() == Some("warning");
                if threshold > 0 && !view.rolled_over {
                    if util >= threshold || blocked {
                        is_over = true;
                    }
                    if util as f64 >= threshold as f64 * GUARDRAIL_WARN_RATIO || warning_status || blocked {
                        is_warning = true;
                    }
                }
                let rank = if threshold > 0 { util - threshold } else { util - 1000 };
                let better = report.as_ref().map(|(r, _, _)| rank > *r).unwrap_or(true);
                if better {
                    report = Some((rank, period, view));
                }
            }
            if let Some((_, period, view)) = report {
                return Ok(BudgetStatus {
                    source: "plan".into(),
                    period: period.into(),
                    percent: view.utilization.round() as i64,
                    is_warning,
                    is_over,
                    reset: view.resets_at,
                    captured_at: view.captured_at,
                    is_stale: view.is_stale,
                    status: view.status,
                    rolled_over: view.rolled_over,
                });
            }
        }
    }

    // 2) No usable plan data for this engine — guardrail inactive.
    Ok(BudgetStatus {
        source: "disabled".into(),
        period: "week".into(),
        percent: 0,
        is_warning: false,
        is_over: false,
        reset: None,
        captured_at: None,
        is_stale: false,
        status: None,
        rolled_over: false,
    })
}

#[tauri::command]
pub async fn get_budget_status(db: State<'_, Db>) -> Result<BudgetStatus, String> {
    // Badge = the account's REAL Claude plan usage (most-constraining window),
    // independent of the Usage budget setting. That setting only gates
    // run-blocking via `enforce_budget`.
    let (plan_json, stale_secs) = read_plan_snapshot(db.inner(), "plan_usage").await?;
    Ok(plan_display_status(plan_json.as_deref(), stale_secs))
}

#[tauri::command]
pub async fn get_codex_budget_status(db: State<'_, Db>) -> Result<BudgetStatus, String> {
    // Same as the Claude badge: the account's REAL Codex rate-limit usage,
    // independent of the Usage budget setting.
    let (plan_json, stale_secs) = read_plan_snapshot(db.inner(), "plan_usage_codex").await?;
    Ok(plan_display_status(plan_json.as_deref(), stale_secs))
}

/// Preflight the run-blocking guardrail for `engine` WITHOUT side effects, so
/// the UI can confirm a budget override BEFORE it optimistically echoes the
/// user's message. Same verdict `enforce_budget` acts on.
#[tauri::command]
pub async fn get_run_budget(db: State<'_, Db>, engine: String) -> Result<BudgetStatus, String> {
    budget_status_for(db.inner(), &engine).await
}

/// Guardrail used at every token-consuming entry point (start / resume /
/// follow-up). Refuses when the budget is over, unless the user explicitly
/// overrode it. The error is prefixed `BUDGET_EXCEEDED` so the UI can offer a
/// one-click override.
pub async fn enforce_budget(db: &Db, engine: &str, override_budget: bool) -> Result<(), String> {
    if override_budget {
        return Ok(());
    }
    let status = budget_status_for(db, engine).await?;
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
        let v = plan_window_by_key("five_hour", &snap, 120).expect("plan window");
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
        let v = plan_window_by_key("five_hour", &snap, 120).expect("plan window");
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
        let v = plan_window_by_key("five_hour", &snap, 120).expect("plan window");
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
        assert!(plan_window_by_key("five_hour", &snap, 120).is_none());
    }

    #[test]
    fn weekly_window_picks_highest_utilization() {
        let now = Utc::now().to_rfc3339();
        let future = (Utc::now() + Duration::hours(48)).to_rfc3339();
        let snap = serde_json::json!({
            "captured_at": now,
            "rate_limits_available": true,
            "windows": {
                "seven_day": { "utilization": 38.0, "resets_at": future },
                "seven_day_opus": { "utilization": 71.0, "resets_at": future },
            },
        });
        let v = weekly_window(&snap, 120).expect("weekly window");
        assert_eq!(v.utilization.round() as i64, 71);
    }
}
