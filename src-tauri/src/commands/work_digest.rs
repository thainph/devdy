//! Work Digest — aggregate runs into a per-day / per-range work summary.
//!
//! `get_work_digest` lists runs (all statuses, including `fetched`) filtered by
//! a local date range and an optional project multi-select, groups them by
//! project, and computes two independent durations per BR-001/BR-005/BR-007:
//!
//! * wall-clock — finished_at − started_at (or now − started_at while running).
//!   Group / global totals are UNION-MERGED so overlapping runs aren't double
//!   counted; each item still reports its own raw wall-clock.
//! * active time — the sum of gaps between consecutive transcript events that
//!   are ≤ the idle threshold (default 300s). Null when no usable timestamps.
//!
//! No LLM is invoked (BR-006): descriptions are derived from title / ref_number
//! / run type that already live in the DB.

use crate::db::Db;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Row, Sqlite};
use std::collections::HashMap;
use std::path::Path;
use tauri::State;

/// Idle threshold for active-time accounting, in seconds (BR-005).
const IDLE_THRESHOLD_SECS: i64 = 300;

#[derive(Debug, Default, Deserialize)]
pub struct WorkDigestFilter {
    /// Inclusive lower bound, `YYYY-MM-DD` (local date from the frontend).
    pub from: Option<String>,
    /// Inclusive upper bound, `YYYY-MM-DD` (local date from the frontend).
    pub to: Option<String>,
    /// Multi-select project filter. `None` → all projects; `Some(empty)` → no
    /// results (an explicit "nothing selected"); `Some(ids)` → those projects.
    pub project_ids: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct WorkItem {
    pub id: String,
    pub project_id: String,
    pub project_name: String,
    pub run_type: String,
    pub ref_number: Option<i64>,
    pub status: String,
    pub engine: String,
    pub title: Option<String>,
    pub description: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub created_at: String,
    pub wall_secs: i64,
    pub active_secs: Option<i64>,
    pub tokens: i64,
    pub cost: f64,
}

#[derive(Debug, Serialize)]
pub struct ProjectGroup {
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub item_count: i64,
    /// Union-merged wall-clock across the group's runs (BR-007).
    pub wall_secs: i64,
    /// Sum of non-null per-item active_secs.
    pub active_secs: i64,
    pub tokens: i64,
    pub cost: f64,
    pub items: Vec<WorkItem>,
}

#[derive(Debug, Default, Serialize)]
pub struct WorkDigestSummary {
    pub total_items: i64,
    /// Union-merged wall-clock across ALL runs (BR-007).
    pub total_wall_secs: i64,
    pub total_active_secs: i64,
    pub total_tokens: i64,
    pub total_cost: f64,
}

#[derive(Debug, Serialize)]
pub struct WorkDigestResult {
    pub summary: WorkDigestSummary,
    pub projects: Vec<ProjectGroup>,
}

/// Aggregated usage (tokens + cost) for a single run, summed over its ledger
/// rows (a resumed run produces multiple `run_usage` entries).
#[derive(Default, Clone, Copy)]
struct UsageAgg {
    tokens: i64,
    cost: f64,
}

/// Union-merge a set of `[start, end]` intervals (in seconds) and return the
/// total covered length. Overlapping or contiguous intervals count once.
fn merge_intervals_secs(intervals: &[(i64, i64)]) -> i64 {
    if intervals.is_empty() {
        return 0;
    }
    let mut sorted: Vec<(i64, i64)> = intervals
        .iter()
        .copied()
        // Guard against inverted / zero-length inputs.
        .filter(|(s, e)| e > s)
        .collect();
    if sorted.is_empty() {
        return 0;
    }
    sorted.sort_by(|a, b| a.0.cmp(&b.0));

    let mut total = 0i64;
    let (mut cur_start, mut cur_end) = sorted[0];
    for &(s, e) in &sorted[1..] {
        if s <= cur_end {
            // Overlapping or contiguous: extend the current window.
            if e > cur_end {
                cur_end = e;
            }
        } else {
            total += cur_end - cur_start;
            cur_start = s;
            cur_end = e;
        }
    }
    total += cur_end - cur_start;
    total
}

/// Sum the gaps between consecutive (already sorted, ascending) epoch-second
/// timestamps, counting only gaps ≤ `idle`. Needs ≥ 2 timestamps, else `None`.
fn active_from_sorted(secs: &[i64], idle: i64) -> Option<i64> {
    if secs.len() < 2 {
        return None;
    }
    let mut total = 0i64;
    for w in secs.windows(2) {
        let gap = w[1] - w[0];
        if gap > 0 && gap <= idle {
            total += gap;
        }
    }
    Some(total)
}

/// Parse an RFC3339 timestamp into epoch seconds.
fn parse_epoch_secs(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.timestamp())
}

/// Per-run wall-clock in seconds (BR-001). `now` is epoch-seconds for running
/// runs that have no finish yet. Never negative.
fn wall_secs_for(
    status: &str,
    started_at: Option<&str>,
    finished_at: Option<&str>,
    now: i64,
) -> i64 {
    let Some(start) = started_at.and_then(parse_epoch_secs) else {
        return 0;
    };
    let end = match finished_at.and_then(parse_epoch_secs) {
        Some(e) => e,
        None if status == "running" => now,
        None => return 0,
    };
    (end - start).max(0)
}

/// The `[start, end]` epoch interval a run contributes to union-merged totals,
/// or `None` when it lacks a usable start (e.g. `fetched`). Running runs use
/// `now` as their (open) end.
fn wall_interval_for(
    status: &str,
    started_at: Option<&str>,
    finished_at: Option<&str>,
    now: i64,
) -> Option<(i64, i64)> {
    let start = started_at.and_then(parse_epoch_secs)?;
    let end = match finished_at.and_then(parse_epoch_secs) {
        Some(e) => e,
        None if status == "running" => now,
        None => return None,
    };
    if end > start {
        Some((start, end))
    } else {
        None
    }
}

/// Human-readable work description without any LLM call (BR-006). Prefers the
/// stored title; otherwise derives from run type + ref_number.
fn describe(run_type: &str, ref_number: Option<i64>, title: Option<&str>) -> String {
    let title = title.map(str::trim).filter(|t| !t.is_empty());
    match run_type {
        "session" => title.unwrap_or("Session").to_string(),
        "analyze_issue" => match (ref_number, title) {
            (Some(n), Some(t)) => format!("Issue #{n} — {t}"),
            (Some(n), None) => format!("Issue #{n}"),
            (None, Some(t)) => t.to_string(),
            (None, None) => "Issue".to_string(),
        },
        "review_pr" => match (ref_number, title) {
            (Some(n), Some(t)) => format!("PR #{n} — {t}"),
            (Some(n), None) => format!("PR #{n}"),
            (None, Some(t)) => t.to_string(),
            (None, None) => "PR".to_string(),
        },
        _ => title.unwrap_or(run_type).to_string(),
    }
}

/// Compute active time (seconds) for a run by scanning its transcript for event
/// timestamps (BR-005). Prefers `transcript_path` (claude JSONL, one object per
/// line with an RFC3339 `timestamp`). The conventional run log at
/// `{project_path}/.devdy/runs/{id}.log` is stream-json without reliable
/// timestamps, so it's skipped when no timestamp parses. Returns `None` when
/// fewer than 2 usable timestamps are found.
fn compute_active_secs(
    run_id: &str,
    project_path: &str,
    transcript_path: Option<&str>,
    idle: i64,
) -> Option<i64> {
    let mut secs = collect_timestamps(transcript_path);
    if secs.len() < 2 {
        // Fall back to the conventional run log; only useful if it happens to
        // carry timestamps (usually it doesn't → returns nothing).
        let conv = Path::new(project_path)
            .join(".devdy")
            .join("runs")
            .join(format!("{run_id}.log"));
        let conv_str = conv.to_string_lossy();
        secs = collect_timestamps(Some(conv_str.as_ref()));
    }
    if secs.len() < 2 {
        return None;
    }
    secs.sort_unstable();
    active_from_sorted(&secs, idle)
}

/// Read a JSONL transcript and collect every parseable `timestamp` field as
/// epoch seconds. Missing file / unreadable → empty vec.
fn collect_timestamps(path: Option<&str>) -> Vec<i64> {
    let Some(path) = path.filter(|p| !p.is_empty()) else {
        return Vec::new();
    };
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(ts) = v.get("timestamp").and_then(|t| t.as_str()) {
            if let Some(epoch) = parse_epoch_secs(ts) {
                out.push(epoch);
            }
        }
    }
    out
}

#[tauri::command]
pub async fn get_work_digest(
    db: State<'_, Db>,
    filter: WorkDigestFilter,
) -> Result<WorkDigestResult, String> {
    // An explicit empty project selection means "show nothing" — don't run a
    // query that would otherwise return everything.
    if matches!(filter.project_ids.as_ref(), Some(ids) if ids.is_empty()) {
        return Ok(WorkDigestResult {
            summary: WorkDigestSummary::default(),
            projects: Vec::new(),
        });
    }

    // ── runs + project name, filtered by derived date + optional projects ──────
    let mut qb = QueryBuilder::<Sqlite>::new(
        "SELECT r.id, r.project_id, p.name AS project_name, r.type, r.ref_number, \
         r.status, r.engine, r.title, r.input_path, r.transcript_path, r.output_path, \
         r.session_id, r.started_at, r.finished_at, r.created_at, p.path AS project_path \
         FROM runs r JOIN projects p ON p.id = r.project_id WHERE 1 = 1",
    );
    if let Some(f) = filter.from.as_ref().filter(|s| !s.is_empty()) {
        qb.push(" AND substr(COALESCE(r.started_at, r.created_at), 1, 10) >= ")
            .push_bind(f.clone());
    }
    if let Some(t) = filter.to.as_ref().filter(|s| !s.is_empty()) {
        qb.push(" AND substr(COALESCE(r.started_at, r.created_at), 1, 10) <= ")
            .push_bind(t.clone());
    }
    if let Some(ids) = filter.project_ids.as_ref().filter(|v| !v.is_empty()) {
        qb.push(" AND r.project_id IN (");
        let mut sep = qb.separated(", ");
        for id in ids {
            sep.push_bind(id.clone());
        }
        qb.push(")");
    }

    let rows = qb
        .build()
        .fetch_all(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    // ── per-run usage aggregate (tokens + cost) ────────────────────────────────
    let usage_rows = sqlx::query(
        "SELECT run_id, COALESCE(SUM(total_tokens), 0) AS tokens, \
         COALESCE(SUM(cost_usd), 0) AS cost FROM run_usage \
         WHERE run_id IS NOT NULL GROUP BY run_id",
    )
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;
    let mut usage: HashMap<String, UsageAgg> = HashMap::new();
    for row in &usage_rows {
        let run_id: String = row.get("run_id");
        let tokens: i64 = row.try_get("tokens").unwrap_or(0);
        let cost: f64 = row.try_get("cost").unwrap_or(0.0);
        usage.insert(run_id, UsageAgg { tokens, cost });
    }

    let now = Utc::now().timestamp();

    // Build items, grouped by project id.
    struct GroupAcc {
        project_id: String,
        project_name: String,
        items: Vec<WorkItem>,
        intervals: Vec<(i64, i64)>,
        active_secs: i64,
        tokens: i64,
        cost: f64,
    }
    let mut groups: HashMap<String, GroupAcc> = HashMap::new();
    let mut global_intervals: Vec<(i64, i64)> = Vec::new();
    let mut summary = WorkDigestSummary::default();

    for row in &rows {
        let id: String = row.get("id");
        let project_id: String = row.get("project_id");
        let project_name: String = row.get("project_name");
        let run_type: String = row.get("type");
        let ref_number: Option<i64> = row.get("ref_number");
        let status: String = row.get("status");
        let engine: String = row.get("engine");
        let title: Option<String> = row.get("title");
        let transcript_path: Option<String> = row.get("transcript_path");
        let project_path: String = row.get("project_path");
        let started_at: Option<String> = row.get("started_at");
        let finished_at: Option<String> = row.get("finished_at");
        let created_at: String = row.get("created_at");

        let wall_secs = wall_secs_for(
            &status,
            started_at.as_deref(),
            finished_at.as_deref(),
            now,
        );
        let active_secs = compute_active_secs(
            &id,
            &project_path,
            transcript_path.as_deref(),
            IDLE_THRESHOLD_SECS,
        );
        let description = describe(&run_type, ref_number, title.as_deref());
        let agg = usage.get(&id).copied().unwrap_or_default();

        let item = WorkItem {
            id: id.clone(),
            project_id: project_id.clone(),
            project_name: project_name.clone(),
            run_type,
            ref_number,
            status: status.clone(),
            engine,
            title,
            description,
            started_at: started_at.clone(),
            finished_at: finished_at.clone(),
            created_at,
            wall_secs,
            active_secs,
            tokens: agg.tokens,
            cost: agg.cost,
        };

        let interval = wall_interval_for(
            &status,
            started_at.as_deref(),
            finished_at.as_deref(),
            now,
        );
        if let Some(iv) = interval {
            global_intervals.push(iv);
        }

        summary.total_items += 1;
        summary.total_tokens += agg.tokens;
        summary.total_cost += agg.cost;
        summary.total_active_secs += active_secs.unwrap_or(0);

        let group = groups.entry(project_id.clone()).or_insert_with(|| GroupAcc {
            project_id,
            project_name,
            items: Vec::new(),
            intervals: Vec::new(),
            active_secs: 0,
            tokens: 0,
            cost: 0.0,
        });
        if let Some(iv) = interval {
            group.intervals.push(iv);
        }
        group.active_secs += active_secs.unwrap_or(0);
        group.tokens += agg.tokens;
        group.cost += agg.cost;
        group.items.push(item);
    }

    summary.total_wall_secs = merge_intervals_secs(&global_intervals);

    // Sort items inside each group by COALESCE(started_at, created_at) desc.
    let mut projects: Vec<ProjectGroup> = groups
        .into_values()
        .map(|mut g| {
            g.items.sort_by(|a, b| {
                let ka = a.started_at.as_deref().unwrap_or(&a.created_at);
                let kb = b.started_at.as_deref().unwrap_or(&b.created_at);
                kb.cmp(ka)
            });
            ProjectGroup {
                project_id: Some(g.project_id),
                project_name: Some(g.project_name),
                item_count: g.items.len() as i64,
                wall_secs: merge_intervals_secs(&g.intervals),
                active_secs: g.active_secs,
                tokens: g.tokens,
                cost: g.cost,
                items: g.items,
            }
        })
        .collect();

    // Groups sorted by total wall-clock desc.
    projects.sort_by(|a, b| b.wall_secs.cmp(&a.wall_secs));

    Ok(WorkDigestResult { summary, projects })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_overlapping_intervals() {
        // AC-12: 09:00–10:00 & 09:30–10:30 overlap → 90 minutes, not 120.
        assert_eq!(merge_intervals_secs(&[(0, 3600), (1800, 5400)]), 5400);
    }

    #[test]
    fn merge_contiguous_intervals() {
        // Touching end-to-start → one continuous 2h window.
        assert_eq!(merge_intervals_secs(&[(0, 3600), (3600, 7200)]), 7200);
    }

    #[test]
    fn merge_disjoint_intervals() {
        // Fully separate → sum of both lengths.
        assert_eq!(merge_intervals_secs(&[(0, 60), (120, 180)]), 120);
    }

    #[test]
    fn merge_empty_is_zero() {
        assert_eq!(merge_intervals_secs(&[]), 0);
    }

    #[test]
    fn active_time_drops_idle_gap() {
        // AC-08: turns at 10:00, 10:02, 10:40, 10:41; idle threshold 5 min.
        // Gaps: 120s (keep), 2280s (drop, > 300), 60s (keep) → 180s = 3m.
        let t0 = 0i64;
        let secs = [t0, t0 + 120, t0 + 120 + 2280, t0 + 120 + 2280 + 60];
        assert_eq!(active_from_sorted(&secs, 300), Some(180));
    }

    #[test]
    fn active_time_needs_two_timestamps() {
        assert_eq!(active_from_sorted(&[], 300), None);
        assert_eq!(active_from_sorted(&[42], 300), None);
    }

    #[test]
    fn active_time_all_within_threshold() {
        assert_eq!(active_from_sorted(&[0, 100, 200, 300], 300), Some(300));
    }

    #[test]
    fn wall_secs_fetched_is_zero() {
        // No started_at → 0 regardless of status.
        assert_eq!(wall_secs_for("fetched", None, None, 1_000), 0);
    }

    #[test]
    fn wall_secs_finished_range() {
        // 09:00 → 09:45 = 2700s.
        let start = "2026-07-14T09:00:00Z";
        let end = "2026-07-14T09:45:00Z";
        assert_eq!(wall_secs_for("done", Some(start), Some(end), 0), 2700);
    }

    #[test]
    fn wall_secs_running_uses_now() {
        let start = "2026-07-14T09:00:00Z";
        let start_epoch = parse_epoch_secs(start).unwrap();
        let now = start_epoch + 600;
        assert_eq!(wall_secs_for("running", Some(start), None, now), 600);
    }

    #[test]
    fn describe_variants() {
        assert_eq!(describe("session", None, None), "Session");
        assert_eq!(describe("session", None, Some("Fix bug")), "Fix bug");
        assert_eq!(describe("analyze_issue", Some(42), None), "Issue #42");
        assert_eq!(
            describe("analyze_issue", Some(42), Some("Login broken")),
            "Issue #42 — Login broken"
        );
        assert_eq!(describe("review_pr", Some(7), None), "PR #7");
    }
}
