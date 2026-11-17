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
//! - today: the `runs` table, local-day boundaries;
//! - external sessions: transcript writes seen by `session_watcher`.

use super::runtime::{self, truncate};
use super::session_watcher;
use crate::db::Db;
use crate::procs::{ProcState, ProcTracker};
use crate::runs::RunRegistry;
use chrono::{DateTime, Duration as ChronoDuration, NaiveTime, TimeZone, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::Row;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
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
    pub external: Vec<ExternalSession>,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRun {
    pub run_id: String,
    pub last_event_at: Option<String>,
    pub current_tool: Option<RuntimeTool>,
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

#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExternalSession {
    pub id: String,
    pub engine: String,
    pub title: String,
    pub cwd: Option<String>,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub last_activity_at: String,
    pub transcript_path: String,
    /// The run the session watcher mirrored this transcript into, if any.
    pub run_id: Option<String>,
}

/// A transcript written within this window counts as a running session.
const EXTERNAL_WINDOW: Duration = Duration::from_secs(120);
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

    let today = today_stats(db.inner(), now.with_timezone(&chrono::Local))
        .await
        .map_err(|e| e.to_string())?;
    let external = external_sessions(db.inner(), &live)
        .await
        .map_err(|e| e.to_string())?;

    Ok(RuntimeSnapshot {
        generated_at: now.to_rfc3339(),
        runs,
        resources,
        today,
        external,
    })
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
        current_tool: a.current_tool().map(|t| RuntimeTool {
            name: t.name.clone(),
            summary: t.summary.clone(),
            started_at: t.started_at.to_rfc3339(),
        }),
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

/// Runs that finished / failed on `now`'s local day, plus the three latest
/// finished, failed or cancelled runs.
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
        "SELECT r.status, r.finished_at FROM runs r WHERE {DEVDY_FINISHED} AND r.finished_at >= ?"
    ))
    .bind(&coarse)
    .fetch_all(db)
    .await?;
    let mut today = RuntimeToday::default();
    for row in rows {
        let finished: String = row.get("finished_at");
        if parse_ts(&finished).is_none_or(|t| t < start) {
            continue;
        }
        match row.get::<String, _>("status").as_str() {
            "done" => today.finished += 1,
            "failed" => today.failed += 1,
            _ => {}
        }
    }

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

// ---- external sessions -------------------------------------------------------

/// What a transcript file says about its session, read once per file.
#[derive(Debug, Clone, PartialEq)]
enum TranscriptMeta {
    /// Not a user session: a codex subagent / fork rollout (part of another
    /// session) or one of Devdy's own helper sessions.
    Skip,
    Cwd(Option<String>),
}

fn meta_cache() -> &'static Mutex<HashMap<PathBuf, TranscriptMeta>> {
    static C: OnceLock<Mutex<HashMap<PathBuf, TranscriptMeta>>> = OnceLock::new();
    C.get_or_init(Mutex::default)
}

const MAX_META_CACHE: usize = 512;

fn is_codex_rollout(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("rollout-"))
}

/// A Claude top-level transcript is `<session uuid>.jsonl` (subagent
/// transcripts are named `agent-*`).
fn claude_session_id(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    uuid::Uuid::parse_str(stem).ok().map(|_| stem.to_string())
}

/// What the head of a Claude transcript says: its first `cwd`, and whether every
/// prompt in it is the `/usage` command (the shape of Devdy's usage probe).
#[derive(Debug, Default, PartialEq)]
struct ClaudeHead {
    cwd: Option<String>,
    only_usage_command: bool,
}

/// Text of a user message's content (string, or its text blocks).
fn user_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Read the head (64 KiB / 200 lines) of a Claude transcript.
fn claude_head(path: &Path) -> ClaudeHead {
    use std::io::{BufRead, BufReader, Read};
    let Ok(file) = std::fs::File::open(path) else {
        return ClaudeHead::default();
    };
    let mut head = ClaudeHead::default();
    let (mut usage_prompts, mut other_prompts) = (0usize, 0usize);
    for line in BufReader::new(file.take(64 * 1024))
        .lines()
        .map_while(Result::ok)
        .take(200)
    {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if head.cwd.is_none() {
            head.cwd = v.get("cwd").and_then(|c| c.as_str()).map(str::to_string);
        }
        if v.get("type").and_then(|t| t.as_str()) != Some("user") {
            continue;
        }
        let Some(content) = v.get("message").and_then(|m| m.get("content")) else {
            continue;
        };
        let text = user_text(content);
        let text = text.trim();
        // Tool results, and the caveat / stdout lines the CLI records around a
        // local slash command, are not prompts.
        if text.is_empty() || text.starts_with("<local-command-") {
            continue;
        }
        if text == "/usage" || text.contains("<command-name>/usage</command-name>") {
            usage_prompts += 1;
        } else {
            other_prompts += 1;
        }
    }
    head.only_usage_command = usage_prompts > 0 && other_prompts == 0;
    head
}

/// The working directories of Devdy's helper sessions (translation, Slack
/// translation, Focus AI, mascot speech, model discovery): they all run in the
/// system temp dir, which macOS may record either way round `/private`.
fn helper_dirs() -> &'static Vec<String> {
    static DIRS: OnceLock<Vec<String>> = OnceLock::new();
    DIRS.get_or_init(|| {
        let tmp = std::env::temp_dir();
        let mut dirs = vec![tmp.to_string_lossy().to_string()];
        if let Ok(real) = std::fs::canonicalize(&tmp) {
            dirs.push(real.to_string_lossy().to_string());
        }
        let extra: Vec<String> = dirs
            .iter()
            .filter_map(|d| match d.strip_prefix("/private") {
                Some(rest) => Some(rest.to_string()),
                None => d.starts_with("/var/").then(|| format!("/private{d}")),
            })
            .collect();
        dirs.extend(extra);
        dirs.iter()
            .map(|d| d.trim_end_matches('/').to_string())
            .filter(|d| !d.is_empty())
            .collect()
    })
}

fn is_helper_cwd(cwd: &str, helper_dirs: &[String]) -> bool {
    let cwd = cwd.trim_end_matches('/');
    helper_dirs.iter().any(|d| d == cwd)
}

/// A transcript's metadata and whether it may be cached. A transcript holding
/// nothing but `/usage` (Devdy's usage probe runs that in a project dir) is
/// skipped but re-read later: a user session may start the same way.
fn read_meta(path: &Path) -> (TranscriptMeta, bool) {
    let helpers = helper_dirs();
    if is_codex_rollout(path) {
        let meta = match crate::commands::codex_sessions::rollout_meta(path) {
            Some((cwd, _)) if is_helper_cwd(&cwd, helpers) => TranscriptMeta::Skip,
            Some((cwd, _)) => TranscriptMeta::Cwd(Some(cwd)),
            None => TranscriptMeta::Skip,
        };
        return (meta, true);
    }
    let head = claude_head(path);
    if head
        .cwd
        .as_deref()
        .is_some_and(|c| is_helper_cwd(c, helpers))
    {
        return (TranscriptMeta::Skip, true);
    }
    if head.only_usage_command {
        return (TranscriptMeta::Skip, false);
    }
    (TranscriptMeta::Cwd(head.cwd), true)
}

/// Metadata of `paths`, reading (off the async runtime) only uncached files.
async fn metas(paths: Vec<PathBuf>) -> HashMap<PathBuf, TranscriptMeta> {
    let mut out = HashMap::new();
    let mut missing = Vec::new();
    {
        let cache = meta_cache().lock().unwrap_or_else(|e| e.into_inner());
        for p in paths {
            match cache.get(&p) {
                Some(m) => {
                    out.insert(p, m.clone());
                }
                None => missing.push(p),
            }
        }
    }
    if missing.is_empty() {
        return out;
    }
    let read = tauri::async_runtime::spawn_blocking(move || {
        missing
            .into_iter()
            .map(|p| {
                let m = read_meta(&p);
                (p, m)
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    let mut cache = meta_cache().lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() + read.len() > MAX_META_CACHE {
        cache.clear();
    }
    for (p, (m, cacheable)) in read {
        if cacheable {
            cache.insert(p.clone(), m.clone());
        }
        out.insert(p, m);
    }
    out
}

#[derive(Debug, Clone)]
struct ProjectRow {
    id: String,
    name: String,
    path: String,
}

/// The project `cwd` is in (deepest match), else the one whose Claude
/// transcript directory is `encoded_dir`.
fn match_project<'a>(
    projects: &'a [ProjectRow],
    cwd: Option<&str>,
    encoded_dir: Option<&str>,
) -> Option<&'a ProjectRow> {
    if let Some(cwd) = cwd {
        let found = projects
            .iter()
            .filter(|p| {
                let root = p.path.trim_end_matches('/');
                !root.is_empty() && (cwd == root || cwd.starts_with(&format!("{root}/")))
            })
            .max_by_key(|p| p.path.len());
        if found.is_some() {
            return found;
        }
    }
    let dir = encoded_dir?;
    projects
        .iter()
        .find(|p| crate::commands::sessions::encode_project_dir(&p.path) == dir)
}

fn dir_basename(path: &str) -> &str {
    let t = path.trim_end_matches('/');
    t.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or(t)
}

/// Session / thread ids of every session Devdy itself ran: any run that went
/// through a Devdy turn (it stamps `started_at` past `created_at`; a session
/// merely imported from an external transcript keeps them equal), any running
/// run, and the live runs. Devdy's sessions write the same transcript stores,
/// so these are never reported as external.
async fn devdy_session_ids(db: &Db, live: &[String]) -> Result<HashSet<String>, sqlx::Error> {
    let mut owned: HashSet<String> = sqlx::query_scalar::<_, String>(
        "SELECT session_id FROM runs WHERE session_id IS NOT NULL \
         AND (status = 'running' OR (started_at IS NOT NULL AND started_at <> created_at))",
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .collect();
    if !live.is_empty() {
        let placeholders = vec!["?"; live.len()].join(", ");
        let sql = format!(
            "SELECT session_id FROM runs WHERE session_id IS NOT NULL AND id IN ({placeholders})"
        );
        let mut q = sqlx::query_scalar::<_, String>(&sql);
        for id in live {
            q = q.bind(id);
        }
        owned.extend(q.fetch_all(db).await?);
    }
    Ok(owned)
}

/// Sessions whose transcript was written in the last [`EXTERNAL_WINDOW`] and
/// that no running Devdy run owns.
async fn external_sessions(db: &Db, live: &[String]) -> Result<Vec<ExternalSession>, sqlx::Error> {
    let writes = session_watcher::recent_writes(EXTERNAL_WINDOW);
    if writes.is_empty() {
        return Ok(Vec::new());
    }
    let owned = devdy_session_ids(db, live).await?;

    let candidates: Vec<(PathBuf, DateTime<Utc>, &'static str, String)> = writes
        .into_iter()
        .filter_map(|(path, at)| {
            let (engine, sid) = if is_codex_rollout(&path) {
                (
                    "codex",
                    crate::commands::codex_sessions::thread_id_from_rollout_name(&path)?,
                )
            } else {
                ("claude", claude_session_id(&path)?)
            };
            (!owned.contains(&sid)).then_some((path, at, engine, sid))
        })
        .collect();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let projects: Vec<ProjectRow> = sqlx::query("SELECT id, name, path FROM projects")
        .fetch_all(db)
        .await?
        .iter()
        .map(|r| ProjectRow {
            id: r.get("id"),
            name: r.get("name"),
            path: r.get("path"),
        })
        .collect();
    let metas = metas(candidates.iter().map(|c| c.0.clone()).collect()).await;

    let sids: Vec<String> = candidates.iter().map(|c| c.3.clone()).collect();
    let mirrored = mirrored_runs(db, &sids).await?;

    let mut out = Vec::new();
    for (path, at, engine, sid) in candidates {
        let cwd = match metas.get(&path) {
            Some(TranscriptMeta::Skip) => continue,
            Some(TranscriptMeta::Cwd(c)) => c.clone(),
            None => None,
        };
        let encoded_dir = (engine == "claude")
            .then(|| {
                path.parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
            })
            .flatten();
        let project = match_project(&projects, cwd.as_deref(), encoded_dir);
        let label = cwd
            .as_deref()
            .map(dir_basename)
            .or(project.map(|p| p.name.as_str()))
            .unwrap_or("?");
        out.push(ExternalSession {
            id: format!("{engine}:{sid}"),
            engine: engine.to_string(),
            title: format!("{engine} · {label}"),
            cwd,
            project_id: project.map(|p| p.id.clone()),
            project_name: project.map(|p| p.name.clone()),
            last_activity_at: at.to_rfc3339(),
            run_id: pick_mirrored_run(
                mirrored.get(&sid).map(Vec::as_slice).unwrap_or_default(),
                &path.to_string_lossy(),
                project.map(|p| p.id.as_str()),
            ),
            transcript_path: path.to_string_lossy().to_string(),
        });
    }
    Ok(out)
}

/// A run row a session was mirrored into by the session watcher.
#[derive(Debug, Clone, PartialEq)]
struct MirroredRun {
    run_id: String,
    project_id: String,
    transcript_path: Option<String>,
}

/// Runs holding each of `session_ids`, most recently active first.
async fn mirrored_runs(
    db: &Db,
    session_ids: &[String],
) -> Result<HashMap<String, Vec<MirroredRun>>, sqlx::Error> {
    let mut out: HashMap<String, Vec<MirroredRun>> = HashMap::new();
    if session_ids.is_empty() {
        return Ok(out);
    }
    let placeholders = vec!["?"; session_ids.len()].join(", ");
    let sql = format!(
        "SELECT id, project_id, session_id, transcript_path FROM runs \
         WHERE session_id IN ({placeholders}) \
         ORDER BY COALESCE(last_activity_at, finished_at, started_at, created_at) DESC, id"
    );
    let mut q = sqlx::query(&sql);
    for sid in session_ids {
        q = q.bind(sid);
    }
    for row in q.fetch_all(db).await? {
        out.entry(row.get("session_id"))
            .or_default()
            .push(MirroredRun {
                run_id: row.get("id"),
                project_id: row.get("project_id"),
                transcript_path: row.get("transcript_path"),
            });
    }
    Ok(out)
}

/// The mirrored run of a transcript: the one recorded with this exact file,
/// else the one in the matched project, else the most recently active.
fn pick_mirrored_run(runs: &[MirroredRun], path: &str, project_id: Option<&str>) -> Option<String> {
    runs.iter()
        .find(|r| r.transcript_path.as_deref() == Some(path))
        .or_else(|| {
            runs.iter()
                .find(|r| Some(r.project_id.as_str()) == project_id)
        })
        .or_else(|| runs.first())
        .map(|r| r.run_id.clone())
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

    #[test]
    fn project_matching() {
        let projects = vec![
            ProjectRow {
                id: "1".into(),
                name: "repo".into(),
                path: "/w/repo".into(),
            },
            ProjectRow {
                id: "2".into(),
                name: "sub".into(),
                path: "/w/repo/sub".into(),
            },
            ProjectRow {
                id: "3".into(),
                name: "other".into(),
                path: "/w/repo-other".into(),
            },
        ];
        let m = |cwd: Option<&str>, dir: Option<&str>| {
            match_project(&projects, cwd, dir).map(|p| p.id.as_str())
        };
        assert_eq!(m(Some("/w/repo"), None), Some("1"));
        assert_eq!(m(Some("/w/repo/.devdy/worktrees/x"), None), Some("1"));
        assert_eq!(m(Some("/w/repo/sub/deep"), None), Some("2"));
        assert_eq!(m(Some("/w/repo-other"), None), Some("3"));
        assert_eq!(m(Some("/elsewhere"), None), None);
        assert_eq!(m(None, Some("-w-repo-other")), Some("3"));
    }

    #[test]
    fn transcript_names() {
        assert!(claude_session_id(Path::new(
            "/h/.claude/projects/-x/0b1c2d3e-0000-4000-8000-123456789abc.jsonl"
        ))
        .is_some());
        assert!(claude_session_id(Path::new(
            "/h/.claude/projects/-x/s/subagents/agent-a1b2.jsonl"
        ))
        .is_none());
        assert!(is_codex_rollout(Path::new(
            "/h/.codex/sessions/2026/11/17/rollout-2026-11-17T01-02-03-abc.jsonl"
        )));
    }

    fn write_tmp(name: &str, body: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("devdy-rt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join(name);
        std::fs::write(&f, body).unwrap();
        (dir, f)
    }

    #[test]
    fn reads_claude_cwd_from_head() {
        let (dir, f) = write_tmp(
            "t.jsonl",
            "{\"type\":\"summary\"}\n{\"type\":\"user\",\"cwd\":\"/w/repo\",\"message\":{\"content\":\"fix the bug\"}}\n",
        );
        let head = claude_head(&f);
        assert_eq!(head.cwd.as_deref(), Some("/w/repo"));
        assert!(!head.only_usage_command);
        assert_eq!(
            read_meta(&f),
            (TranscriptMeta::Cwd(Some("/w/repo".into())), true)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_helper_sessions_in_temp_dir() {
        let tmp = std::env::temp_dir();
        let tmp = tmp.to_string_lossy();
        let helpers = helper_dirs();
        assert!(is_helper_cwd(&tmp, helpers));
        assert!(is_helper_cwd(tmp.trim_end_matches('/'), helpers));
        if let Ok(real) = std::fs::canonicalize(std::env::temp_dir()) {
            assert!(is_helper_cwd(&real.to_string_lossy(), helpers));
        }
        assert!(!is_helper_cwd("/w/repo", helpers));
        assert!(!is_helper_cwd(
            &format!("{}/project", tmp.trim_end_matches('/')),
            helpers
        ));

        let line = serde_json::json!({"type": "user", "cwd": tmp, "message": {"content": "Translate this"}});
        let (dir, f) = write_tmp("t.jsonl", &format!("{line}\n"));
        assert_eq!(read_meta(&f), (TranscriptMeta::Skip, true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn usage_probe_transcript_is_skipped_until_a_real_prompt() {
        let probe = concat!(
            "{\"type\":\"user\",\"cwd\":\"/w/repo\",\"isMeta\":true,\"message\":{\"content\":\"<local-command-caveat>Caveat</local-command-caveat>\"}}\n",
            "{\"type\":\"user\",\"cwd\":\"/w/repo\",\"message\":{\"content\":\"<command-name>/usage</command-name>\\n<command-args></command-args>\"}}\n",
            "{\"type\":\"user\",\"cwd\":\"/w/repo\",\"message\":{\"content\":\"<local-command-stdout>42%</local-command-stdout>\"}}\n",
        );
        let (dir, f) = write_tmp("t.jsonl", probe);
        assert_eq!(read_meta(&f), (TranscriptMeta::Skip, false), "not cached");
        // The user continues that session: it is theirs.
        let more = "{\"type\":\"user\",\"cwd\":\"/w/repo\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"now refactor\"}]}}\n";
        std::fs::write(&f, format!("{probe}{more}")).unwrap();
        assert_eq!(read_meta(&f).0, TranscriptMeta::Cwd(Some("/w/repo".into())));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn finds_the_run_a_transcript_was_mirrored_into() {
        let db = memory_db().await;
        let at = "2026-11-17T00:00:00+00:00";
        for (id, project, sid, path, activity) in [
            (
                "imp-a",
                "p1",
                "sid-a",
                Some("/h/.claude/projects/-w-a/sid-a.jsonl"),
                "2026-11-17T01:00:00+00:00",
            ),
            ("imp-b1", "p1", "sid-b", None, "2026-11-17T01:00:00+00:00"),
            ("imp-b2", "p2", "sid-b", None, "2026-11-17T02:00:00+00:00"),
            ("other", "p1", "sid-x", None, "2026-11-17T03:00:00+00:00"),
        ] {
            sqlx::query(
                "INSERT INTO runs (id, project_id, type, status, engine, session_id, transcript_path, \
                 created_at, started_at, finished_at, last_activity_at) \
                 VALUES (?, ?, 'session', 'done', 'claude', ?, ?, ?, ?, ?, ?)",
            )
            .bind(id)
            .bind(project)
            .bind(sid)
            .bind(path)
            .bind(at)
            .bind(at)
            .bind(at)
            .bind(activity)
            .execute(&db)
            .await
            .unwrap();
        }
        let sids: Vec<String> = ["sid-a", "sid-b", "sid-none"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let m = mirrored_runs(&db, &sids).await.unwrap();
        assert_eq!(m.len(), 2);
        assert!(!m.contains_key("sid-x"));
        let b = &m["sid-b"];
        assert_eq!(b[0].run_id, "imp-b2", "most recently active first");

        let a = &m["sid-a"];
        assert_eq!(
            pick_mirrored_run(a, "/h/.claude/projects/-w-a/sid-a.jsonl", None).as_deref(),
            Some("imp-a")
        );
        assert_eq!(
            pick_mirrored_run(b, "/elsewhere.jsonl", Some("p1")).as_deref(),
            Some("imp-b1")
        );
        assert_eq!(
            pick_mirrored_run(b, "/elsewhere.jsonl", None).as_deref(),
            Some("imp-b2")
        );
        assert_eq!(pick_mirrored_run(&[], "/x.jsonl", Some("p1")), None);
        assert!(mirrored_runs(&db, &[]).await.unwrap().is_empty());

        let s = ExternalSession {
            id: "claude:sid-a".into(),
            engine: "claude".into(),
            title: "claude · a".into(),
            cwd: None,
            project_id: None,
            project_name: None,
            last_activity_at: at.into(),
            transcript_path: "/x".into(),
            run_id: Some("imp-a".into()),
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["runId"], "imp-a");
        assert_eq!(v["transcriptPath"], "/x");
    }

    #[tokio::test]
    async fn devdy_sessions_are_owned_imports_are_not() {
        let db = memory_db().await;
        let created = "2026-11-16T00:00:00+00:00";
        insert_run(
            &db,
            "devdy-done",
            "done",
            created,
            Some("2026-11-16T01:00:00+00:00"),
            Some("2026-11-16T02:00:00+00:00"),
        )
        .await;
        insert_run(
            &db,
            "devdy-running",
            "running",
            created,
            Some(created),
            None,
        )
        .await;
        insert_run(
            &db,
            "imported",
            "done",
            created,
            Some(created),
            Some("2026-11-17T00:00:00+00:00"),
        )
        .await;
        insert_run(&db, "live", "done", created, Some(created), None).await;
        insert_run(
            &db,
            "no-session",
            "done",
            created,
            Some("2026-11-16T01:00:00+00:00"),
            None,
        )
        .await;
        for id in ["devdy-done", "devdy-running", "imported", "live"] {
            sqlx::query("UPDATE runs SET session_id = ? WHERE id = ?")
                .bind(format!("sid-{id}"))
                .bind(id)
                .execute(&db)
                .await
                .unwrap();
        }
        let owned = devdy_session_ids(&db, &[]).await.unwrap();
        assert!(owned.contains("sid-devdy-done"), "finished Devdy runs too");
        assert!(owned.contains("sid-devdy-running"));
        assert!(
            !owned.contains("sid-imported"),
            "an external session imported into History is still external"
        );
        assert!(!owned.contains("sid-live"));
        let owned = devdy_session_ids(&db, &["live".to_string()]).await.unwrap();
        assert!(owned.contains("sid-live"));
        assert_eq!(owned.len(), 3);
    }
}
