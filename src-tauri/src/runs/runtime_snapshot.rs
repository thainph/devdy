//! `get_runtime_snapshot`: everything the Runtime Dashboard shows that the
//! webview cannot derive itself, in one cheap call (polled every few seconds
//! while the dashboard is visible). The webview mirrors this shape in
//! `src/lib/runtimeSnapshot.ts`.
//!
//! Sources, all already in memory or one small query away — no log re-reads
//! and no `ps` call of its own (the background-process sampler refreshes the
//! tracker every few seconds):
//! - live runs: the `RunRegistry` + per-run activity fed by the drain
//!   (`runs::runtime`), open AskUserQuestion prompts from the permission store,
//!   CPU / memory of each run's process tree from the `ProcTracker`;
//! - today: the `runs` table, local-day boundaries.

use super::runtime::{self, truncate};
use crate::db::Db;
use crate::procs::{ProcState, ProcTracker};
use crate::runs::RunRegistry;
use chrono::{DateTime, Duration as ChronoDuration, NaiveTime, TimeZone, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::Row;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, State};

#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSnapshot {
    pub generated_at: String,
    pub runs: Vec<RuntimeRun>,
    pub resources: RuntimeResources,
    pub today: RuntimeToday,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRun {
    pub run_id: String,
    pub last_event_at: Option<String>,
    pub current_tool: Option<RuntimeTool>,
    /// The latest tool call, finished or not (shown when none is running).
    pub last_tool: Option<RuntimeTool>,
    pub events_per_minute: Vec<u32>,
    pub context_tokens: Option<u64>,
    pub context_window: Option<u64>,
    pub awaiting_question: Option<AwaitingQuestion>,
    pub cpu_percent: f64,
    pub rss_kb: u64,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTool {
    pub name: String,
    pub summary: String,
    pub started_at: String,
}

fn runtime_tool(t: &runtime::OpenTool) -> RuntimeTool {
    RuntimeTool {
        name: t.name.clone(),
        summary: t.summary.clone(),
        started_at: t.started_at.to_rfc3339(),
    }
}

#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AwaitingQuestion {
    pub question: String,
    pub options: Vec<String>,
    pub since: String,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeResources {
    pub cpu_percent: f64,
    pub rss_kb: u64,
    pub total_mem_kb: u64,
    pub process_count: usize,
    pub orphan_count: usize,
    pub stale_worktree_count: Option<usize>,
    pub cpu_history: Vec<f64>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeToday {
    pub finished: usize,
    pub failed: usize,
    pub recent: Vec<RecentRun>,
    /// Every run finished today (local day), oldest first.
    pub timeline: Vec<TimelineRun>,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TimelineRun {
    pub run_id: String,
    pub title: Option<String>,
    pub finished_at: String,
    pub status: String,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecentRun {
    pub run_id: String,
    pub project_id: String,
    pub project_name: String,
    pub title: Option<String>,
    pub status: String,
    pub error: Option<String>,
    pub finished_at: String,
}

/// CPU samples kept for the sparkline.
const CPU_HISTORY: usize = 12;
/// Snapshots closer together than this (two windows polling) update the
/// newest CPU sample instead of adding one.
const CPU_SAMPLE_MIN_GAP: Duration = Duration::from_secs(2);
/// Longest AskUserQuestion text returned.
const QUESTION_MAX: usize = 300;
const OPTION_MAX: usize = 80;

#[tauri::command]
pub async fn get_runtime_snapshot(
    app: AppHandle,
    db: State<'_, Db>,
    registry: State<'_, RunRegistry>,
) -> Result<RuntimeSnapshot, String> {
    let now = Utc::now();
    let mut live: Vec<String> = registry.lock().await.keys().cloned().collect();
    live.sort();
    let live_set: HashSet<String> = live.iter().cloned().collect();
    runtime::prune(&live_set);

    let views = app
        .try_state::<ProcTracker>()
        .map(|t| t.view())
        .unwrap_or_default();
    let procs = summarize_procs(&views);

    let questions = open_questions();
    let runs = live
        .iter()
        .map(|id| {
            let activity = runtime::get(id).unwrap_or_default();
            let (cpu, rss) = procs.per_run.get(id).copied().unwrap_or((0.0, 0));
            build_run(id, &activity, questions.get(id).cloned(), cpu, rss, now)
        })
        .collect();

    let resources = RuntimeResources {
        cpu_percent: round1(procs.cpu),
        rss_kb: procs.rss_kb,
        total_mem_kb: total_mem_kb(),
        process_count: procs.process_count,
        orphan_count: procs.orphan_count,
        // No source tracks abandoned worktrees yet.
        stale_worktree_count: None,
        cpu_history: push_cpu_sample(round1(procs.cpu)),
    };

    let today = today_section(db.inner(), now.with_timezone(&chrono::Local)).await;

    Ok(RuntimeSnapshot {
        generated_at: now.to_rfc3339(),
        runs,
        resources,
        today,
    })
}

// Each section degrades on its own: a failed query empties that section instead
// of failing the whole snapshot. Only the error kind is logged.

async fn today_section<Tz: TimeZone>(db: &Db, now: DateTime<Tz>) -> RuntimeToday {
    today_stats(db, now).await.unwrap_or_else(|e| {
        tracing::warn!(
            event = "runtime_snapshot_today_failed",
            error = sql_error_kind(&e)
        );
        RuntimeToday::default()
    })
}

/// A short, data-free description of a query failure for the log.
fn sql_error_kind(e: &sqlx::Error) -> &'static str {
    match e {
        sqlx::Error::Database(_) => "database error",
        sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => "pool unavailable",
        sqlx::Error::Io(_) => "io error",
        sqlx::Error::ColumnDecode { .. } | sqlx::Error::Decode(_) => "decode error",
        _ => "query error",
    }
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

fn build_run(
    run_id: &str,
    a: &runtime::RunActivity,
    awaiting_question: Option<AwaitingQuestion>,
    cpu: f64,
    rss_kb: u64,
    now: DateTime<Utc>,
) -> RuntimeRun {
    RuntimeRun {
        run_id: run_id.to_string(),
        last_event_at: a.last_event_at.map(|t| t.to_rfc3339()),
        current_tool: a.current_tool().map(runtime_tool),
        last_tool: a.last_tool().map(runtime_tool),
        events_per_minute: a.events_per_minute(now),
        context_tokens: a.context_tokens,
        // The stream reports the model without its context variant (`[1m]`),
        // so the webview's own table (which knows the selection) decides.
        context_window: None,
        awaiting_question,
        cpu_percent: round1(cpu),
        rss_kb,
    }
}

// ---- processes ---------------------------------------------------------------

#[derive(Debug, Default, PartialEq)]
struct ProcSummary {
    cpu: f64,
    rss_kb: u64,
    process_count: usize,
    /// Leftover + orphan entries (processes whose owner is gone), as counted by
    /// the Background Processes view.
    orphan_count: usize,
    /// run id → (cpu %, rss KiB) of its live process tree.
    per_run: HashMap<String, (f64, u64)>,
}

fn summarize_procs(views: &[crate::procs::EntryView]) -> ProcSummary {
    let mut s = ProcSummary::default();
    for v in views {
        let cpu: f64 = v.processes.iter().map(|p| p.cpu as f64).sum();
        let rss: u64 = v.processes.iter().map(|p| p.rss_kb).sum();
        s.cpu += cpu;
        s.rss_kb += rss;
        s.process_count += v.processes.len();
        match v.state {
            ProcState::Active => {
                if let Some(run_id) = &v.run_id {
                    let e = s.per_run.entry(run_id.clone()).or_default();
                    e.0 += cpu;
                    e.1 += rss;
                }
            }
            ProcState::Leftover | ProcState::Orphan => s.orphan_count += 1,
        }
    }
    s
}

fn cpu_history() -> &'static Mutex<(VecDeque<f64>, Option<Instant>)> {
    static H: OnceLock<Mutex<(VecDeque<f64>, Option<Instant>)>> = OnceLock::new();
    H.get_or_init(Mutex::default)
}

/// Record this snapshot's total CPU and return the history, oldest first.
fn push_cpu_sample(cpu: f64) -> Vec<f64> {
    let mut h = cpu_history().lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    let (samples, last) = &mut *h;
    push_sample(samples, last, cpu, now);
    samples.iter().copied().collect()
}

fn push_sample(samples: &mut VecDeque<f64>, last: &mut Option<Instant>, cpu: f64, now: Instant) {
    let close = last.is_some_and(|t| now.duration_since(t) < CPU_SAMPLE_MIN_GAP);
    if close && !samples.is_empty() {
        if let Some(back) = samples.back_mut() {
            *back = cpu;
        }
        return;
    }
    samples.push_back(cpu);
    while samples.len() > CPU_HISTORY {
        samples.pop_front();
    }
    *last = Some(now);
}

/// Physical memory of the machine in KiB (0 when unknown). Read once.
fn total_mem_kb() -> u64 {
    static TOTAL: OnceLock<u64> = OnceLock::new();
    *TOTAL.get_or_init(read_total_mem_bytes) / 1024
}

#[cfg(target_os = "macos")]
fn read_total_mem_bytes() -> u64 {
    let mut value: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    // SAFETY: `hw.memsize` is a 64-bit integer; buffer and length match it.
    let rc = unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            &mut value as *mut u64 as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc == 0 {
        value
    } else {
        0
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn read_total_mem_bytes() -> u64 {
    // SAFETY: plain sysconf queries.
    let (pages, size) = unsafe {
        (
            libc::sysconf(libc::_SC_PHYS_PAGES),
            libc::sysconf(libc::_SC_PAGESIZE),
        )
    };
    if pages > 0 && size > 0 {
        pages as u64 * size as u64
    } else {
        0
    }
}

#[cfg(not(unix))]
fn read_total_mem_bytes() -> u64 {
    0
}

// ---- AskUserQuestion ---------------------------------------------------------

/// The oldest open AskUserQuestion prompt of each run.
fn open_questions() -> HashMap<String, AwaitingQuestion> {
    let mut out: HashMap<String, (DateTime<Utc>, AwaitingQuestion)> = HashMap::new();
    for p in super::permission::all_pending() {
        if p.event.tool_name != "AskUserQuestion" {
            continue;
        }
        let q = question_of(
            &p.event.tool_input,
            p.event.description.as_deref(),
            p.requested_at,
        );
        match out.get(&p.event.run_id) {
            Some((at, _)) if *at <= p.requested_at => {}
            _ => {
                out.insert(p.event.run_id.clone(), (p.requested_at, q));
            }
        }
    }
    out.into_iter().map(|(k, (_, q))| (k, q)).collect()
}

/// First question of an AskUserQuestion input, with its option labels.
fn question_of(input: &Value, fallback: Option<&str>, since: DateTime<Utc>) -> AwaitingQuestion {
    let first = input
        .get("questions")
        .and_then(|q| q.as_array())
        .and_then(|q| q.first());
    let question = first
        .and_then(|q| q.get("question"))
        .and_then(|q| q.as_str())
        .or(fallback)
        .unwrap_or("");
    let options = first
        .and_then(|q| q.get("options"))
        .and_then(|o| o.as_array())
        .map(|opts| {
            opts.iter()
                .filter_map(|o| {
                    o.get("label")
                        .and_then(|l| l.as_str())
                        .or_else(|| o.as_str())
                })
                .map(|l| truncate(l.trim(), OPTION_MAX))
                .collect()
        })
        .unwrap_or_default();
    AwaitingQuestion {
        question: truncate(question.trim(), QUESTION_MAX),
        options,
        since: since.to_rfc3339(),
    }
}

// ---- today -------------------------------------------------------------------

/// Start of `now`'s local day, in UTC.
fn day_start<Tz: TimeZone>(now: &DateTime<Tz>) -> DateTime<Utc> {
    let tz = now.timezone();
    let midnight = now.date_naive().and_time(NaiveTime::MIN);
    tz.from_local_datetime(&midnight)
        .earliest()
        // A DST jump skipping midnight: the day starts at the first valid instant.
        .or_else(|| {
            tz.from_local_datetime(&(midnight + ChronoDuration::hours(1)))
                .earliest()
        })
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|| now.with_timezone(&Utc))
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Runs a Devdy turn finished, excluding sessions merely imported from an
/// external transcript (inserted already `done` with `started_at = created_at`;
/// every Devdy turn stamps its own `started_at`).
const DEVDY_FINISHED: &str =
    "r.status IN ('done', 'failed', 'cancelled') AND r.finished_at IS NOT NULL \
     AND r.started_at IS NOT NULL AND r.started_at <> r.created_at";

/// Runs that finished / failed on `now`'s local day (counts and a timeline,
/// oldest first), plus the three latest finished, failed or cancelled runs.
pub async fn today_stats<Tz: TimeZone>(
    db: &Db,
    now: DateTime<Tz>,
) -> Result<RuntimeToday, sqlx::Error> {
    let start = day_start(&now);
    // Stored stamps are UTC RFC3339; a coarse string bound a day early lets the
    // query skip old rows, the exact local-day cut is made after parsing.
    let coarse = (start - ChronoDuration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let rows = sqlx::query(&format!(
        "SELECT r.id, r.title, r.status, r.finished_at FROM runs r \
         WHERE {DEVDY_FINISHED} AND r.finished_at >= ?"
    ))
    .bind(&coarse)
    .fetch_all(db)
    .await?;
    let mut today = RuntimeToday::default();
    let mut timeline: Vec<(DateTime<Utc>, TimelineRun)> = Vec::new();
    for row in rows {
        let finished: String = row.get("finished_at");
        let Some(at) = parse_ts(&finished).filter(|t| *t >= start) else {
            continue;
        };
        let status: String = row.get("status");
        match status.as_str() {
            "done" => today.finished += 1,
            "failed" => today.failed += 1,
            _ => {}
        }
        timeline.push((
            at,
            TimelineRun {
                run_id: row.get("id"),
                title: row.get("title"),
                finished_at: at.to_rfc3339(),
                status,
            },
        ));
    }
    timeline.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.run_id.cmp(&b.1.run_id)));
    today.timeline = timeline.into_iter().map(|(_, r)| r).collect();

    let recent = sqlx::query(&format!(
        "SELECT r.id, r.project_id, p.name AS project_name, r.title, r.status, r.finished_at \
         FROM runs r JOIN projects p ON p.id = r.project_id \
         WHERE {DEVDY_FINISHED} ORDER BY r.finished_at DESC LIMIT 3"
    ))
    .fetch_all(db)
    .await?;
    today.recent = recent
        .iter()
        .map(|row| {
            let run_id: String = row.get("id");
            let status: String = row.get("status");
            let finished_at: String = row.get("finished_at");
            RecentRun {
                error: (status == "failed")
                    .then(|| runtime::last_error(&run_id))
                    .flatten(),
                project_id: row.get("project_id"),
                project_name: row.get("project_name"),
                title: row.get("title"),
                finished_at: parse_ts(&finished_at)
                    .map(|t| t.to_rfc3339())
                    .unwrap_or(finished_at),
                status,
                run_id,
            }
        })
        .collect();
    Ok(today)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procs::{EntryView, ProcKind};
    use crate::slack_threads::tests::memory_db;
    use chrono::FixedOffset;
    use serde_json::json;

    async fn insert_run(
        db: &Db,
        id: &str,
        status: &str,
        created: &str,
        started: Option<&str>,
        finished: Option<&str>,
    ) {
        sqlx::query(
            "INSERT INTO runs (id, project_id, type, status, engine, created_at, started_at, finished_at, title) \
             VALUES (?, 'p1', 'session', ?, 'claude', ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(status)
        .bind(created)
        .bind(started)
        .bind(finished)
        .bind(format!("Run {id}"))
        .execute(db)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn today_counts_local_day_and_lists_latest() {
        let db = memory_db().await;
        let created = "2026-11-16T00:00:00+00:00";
        let started = Some("2026-11-16T01:00:00+00:00");
        // Local zone UTC+7: 2026-11-17 local starts at 2026-11-16T17:00Z.
        insert_run(
            &db,
            "d1",
            "done",
            created,
            started,
            Some("2026-11-16T18:00:00.123456+00:00"),
        )
        .await;
        insert_run(
            &db,
            "d2",
            "done",
            created,
            started,
            Some("2026-11-17T02:00:00Z"),
        )
        .await;
        insert_run(
            &db,
            "f1",
            "failed",
            created,
            started,
            Some("2026-11-17T03:00:00+00:00"),
        )
        .await;
        insert_run(
            &db,
            "c1",
            "cancelled",
            created,
            started,
            Some("2026-11-17T04:00:00+00:00"),
        )
        .await;
        // Finished before local midnight (yesterday locally, today in UTC terms? no: 16:59Z).
        insert_run(
            &db,
            "old",
            "done",
            created,
            started,
            Some("2026-11-16T16:59:00+00:00"),
        )
        .await;
        // Still running / never finished.
        insert_run(&db, "run", "running", created, started, None).await;
        // Imported external transcript: started_at == created_at.
        insert_run(
            &db,
            "imp",
            "done",
            "2026-11-17T01:00:00+00:00",
            Some("2026-11-17T01:00:00+00:00"),
            Some("2026-11-17T05:00:00+00:00"),
        )
        .await;

        let tz = FixedOffset::east_opt(7 * 3600).unwrap();
        let now = tz.with_ymd_and_hms(2026, 11, 17, 13, 0, 0).unwrap();
        runtime::end("f1", "failed", Some("Engine exited with code 1"));
        let t = today_stats(&db, now).await.unwrap();
        assert_eq!(t.finished, 2, "d1 + d2");
        assert_eq!(t.failed, 1);
        let ids: Vec<&str> = t.recent.iter().map(|r| r.run_id.as_str()).collect();
        assert_eq!(ids, ["c1", "f1", "d2"]);
        assert_eq!(t.recent[0].status, "cancelled");
        assert_eq!(
            t.recent[1].error.as_deref(),
            Some("Engine exited with code 1")
        );
        assert_eq!(t.recent[2].error, None);
        assert_eq!(t.recent[2].project_name, "proj");
        assert_eq!(t.recent[2].title.as_deref(), Some("Run d2"));
        assert!(t.recent[2].finished_at.starts_with("2026-11-17T02:00:00"));
    }

    #[tokio::test]
    async fn today_timeline_lists_runs_finished_today_oldest_first() {
        let db = memory_db().await;
        let created = "2026-11-16T00:00:00+00:00";
        let started = Some("2026-11-16T01:00:00+00:00");
        // Inserted out of order; local zone UTC+7 (the day starts at 2026-11-16T17:00Z).
        insert_run(
            &db,
            "f1",
            "failed",
            created,
            started,
            Some("2026-11-17T03:00:00+00:00"),
        )
        .await;
        insert_run(
            &db,
            "d1",
            "done",
            created,
            started,
            Some("2026-11-16T18:00:00.5+00:00"),
        )
        .await;
        insert_run(
            &db,
            "c1",
            "cancelled",
            created,
            started,
            Some("2026-11-17T04:00:00Z"),
        )
        .await;
        insert_run(
            &db,
            "old",
            "done",
            created,
            started,
            Some("2026-11-16T16:59:00+00:00"),
        )
        .await;
        insert_run(&db, "run", "running", created, started, None).await;
        insert_run(
            &db,
            "imp",
            "done",
            "2026-11-17T01:00:00+00:00",
            Some("2026-11-17T01:00:00+00:00"),
            Some("2026-11-17T05:00:00+00:00"),
        )
        .await;

        let tz = FixedOffset::east_opt(7 * 3600).unwrap();
        let now = tz.with_ymd_and_hms(2026, 11, 17, 13, 0, 0).unwrap();
        let t = today_stats(&db, now).await.unwrap();
        let ids: Vec<&str> = t.timeline.iter().map(|r| r.run_id.as_str()).collect();
        assert_eq!(
            ids,
            ["d1", "f1", "c1"],
            "today only, no imports, oldest first"
        );
        let statuses: Vec<&str> = t.timeline.iter().map(|r| r.status.as_str()).collect();
        assert_eq!(statuses, ["done", "failed", "cancelled"]);
        assert_eq!(t.timeline[0].title.as_deref(), Some("Run d1"));
        assert!(t.timeline[0]
            .finished_at
            .starts_with("2026-11-16T18:00:00.5"));
        assert!(t.timeline[2].finished_at.starts_with("2026-11-17T04:00:00"));
        assert_eq!((t.finished, t.failed), (1, 1));
    }

    #[tokio::test]
    async fn sections_degrade_independently() {
        let db = memory_db().await;
        insert_run(
            &db,
            "d",
            "done",
            "2026-11-16T00:00:00+00:00",
            Some("2026-11-16T01:00:00+00:00"),
            Some("2026-11-17T02:00:00+00:00"),
        )
        .await;
        assert_eq!(today_section(&db, Utc::now()).await.recent.len(), 1);
        db.close().await;
        assert!(today_stats(&db, Utc::now()).await.is_err());
        assert_eq!(
            today_section(&db, Utc::now()).await,
            RuntimeToday::default()
        );
    }

    #[tokio::test]
    async fn today_is_empty_without_runs() {
        let db = memory_db().await;
        let t = today_stats(&db, Utc::now()).await.unwrap();
        assert_eq!(t, RuntimeToday::default());
    }

    #[test]
    fn local_day_start() {
        let tz = FixedOffset::west_opt(5 * 3600).unwrap();
        let now = tz.with_ymd_and_hms(2026, 3, 1, 0, 30, 0).unwrap();
        assert_eq!(day_start(&now).to_rfc3339(), "2026-03-01T05:00:00+00:00");
    }

    fn row(pid: u32, cpu: f32, rss: u64) -> crate::procs::ps::PsRow {
        crate::procs::ps::PsRow {
            pid,
            ppid: 1,
            pgid: pid,
            start: String::new(),
            rss_kb: rss,
            cpu,
            stat: "S".into(),
            command: "node".into(),
        }
    }

    fn view(run: Option<&str>, state: ProcState, procs: Vec<crate::procs::ps::PsRow>) -> EntryView {
        EntryView {
            id: uuid::Uuid::new_v4().to_string(),
            kind: if run.is_some() {
                ProcKind::Run
            } else {
                ProcKind::Translate
            },
            state,
            label: "x".into(),
            run_id: run.map(str::to_string),
            project_id: None,
            root_pid: procs.first().map_or(0, |p| p.pid),
            created_at: String::new(),
            released_at: None,
            processes: procs,
        }
    }

    #[test]
    fn process_totals_per_run_and_orphans() {
        let views = vec![
            view(
                Some("a"),
                ProcState::Active,
                vec![row(1, 10.0, 100), row(2, 5.5, 50)],
            ),
            view(None, ProcState::Active, vec![row(3, 1.0, 10)]),
            view(Some("b"), ProcState::Leftover, vec![row(4, 2.0, 20)]),
            view(
                Some("c"),
                ProcState::Orphan,
                vec![row(5, 0.0, 5), row(6, 0.0, 5)],
            ),
        ];
        let s = summarize_procs(&views);
        assert_eq!(s.process_count, 6);
        assert_eq!(s.rss_kb, 190);
        assert!((s.cpu - 18.5).abs() < 1e-6);
        assert_eq!(s.orphan_count, 2);
        assert_eq!(s.per_run.get("a"), Some(&(15.5, 150)));
        assert!(
            !s.per_run.contains_key("b"),
            "a leftover is not the live run's"
        );
    }

    #[test]
    fn cpu_history_is_bounded_and_coalesces_close_samples() {
        let mut samples = VecDeque::new();
        let mut last = None;
        let t0 = Instant::now();
        for i in 0..20 {
            push_sample(
                &mut samples,
                &mut last,
                i as f64,
                t0 + Duration::from_secs(3 * i),
            );
        }
        assert_eq!(samples.len(), CPU_HISTORY);
        assert_eq!(samples.front(), Some(&8.0));
        assert_eq!(samples.back(), Some(&19.0));
        // A second window polling right after replaces the newest sample.
        push_sample(
            &mut samples,
            &mut last,
            99.0,
            t0 + Duration::from_secs(57) + Duration::from_millis(500),
        );
        assert_eq!(samples.len(), CPU_HISTORY);
        assert_eq!(samples.back(), Some(&99.0));
    }

    #[test]
    fn total_memory_is_known_on_unix() {
        #[cfg(unix)]
        assert!(total_mem_kb() > 0);
    }

    #[test]
    fn question_with_options() {
        let since = Utc::now();
        let q = question_of(
            &json!({"questions": [{"question": "  Which DB? ", "options": [{"label": "Postgres"}, {"label": "SQLite"}]}]}),
            None,
            since,
        );
        assert_eq!(q.question, "Which DB?");
        assert_eq!(q.options, ["Postgres", "SQLite"]);
        let q = question_of(&json!({}), Some("MCP asks"), since);
        assert_eq!(q.question, "MCP asks");
        assert!(q.options.is_empty());
    }
}
