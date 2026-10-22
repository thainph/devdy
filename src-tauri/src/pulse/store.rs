//! SQL for Project Pulse: watch rules (BR-01/03/04), project lookup, the
//! overview read and the BR-11 request queue. Errors are `CodedError`s.

use super::content::SlackMsg;
use super::digest::DigestDraft;
use super::health::{self, DigestFacts, DueItem, ProjectHealth, WatchFacts, WatchHealth};
use super::source::{parse_source, Source, SourceError};
use crate::db::Db;
use crate::slack_threads::mrkdwn::Names;
use crate::slack_threads::store::{internal, is_safe_id};
use crate::slack_threads::CodedError;
use serde::Serialize;
use std::collections::HashMap;
use uuid::Uuid;

pub const VIAS: [&str; 3] = ["mcp", "button", "paste"];

const WATCH_COLS: &str =
    "w.id, w.project_id, p.name AS project_name, w.source_type, w.source_ref, \
     w.source_url, w.slack_thread_id, w.title, w.active, w.added_via, w.added_run_id, \
     w.resolve_status, w.last_error, w.checked_at, w.created_at, w.updated_at";
const WATCH_FROM: &str = "pulse_watches w JOIN projects p ON p.id = w.project_id";

#[derive(Debug, Clone, Serialize, sqlx::FromRow, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Watch {
    pub id: String,
    pub project_id: String,
    pub project_name: String,
    pub source_type: String,
    pub source_ref: String,
    pub source_url: String,
    pub slack_thread_id: Option<String>,
    pub title: Option<String>,
    pub active: bool,
    pub added_via: String,
    pub added_run_id: Option<String>,
    pub resolve_status: String,
    pub last_error: Option<String>,
    pub checked_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Digest {
    pub id: String,
    pub watch_id: String,
    pub content_hash: String,
    pub status: String,
    pub ball: String,
    pub ball_since: Option<String>,
    pub summary: String,
    pub next_action: Option<String>,
    pub deadlines: serde_json::Value,
    pub decisions: serde_json::Value,
    pub open_questions: serde_json::Value,
    pub truncated: bool,
    /// The GitHub issue was closed when this digest was made (BR-12).
    pub source_closed: bool,
    pub model: Option<String>,
    pub created_at: String,
}

#[derive(sqlx::FromRow)]
struct DigestRow {
    id: String,
    watch_id: String,
    content_hash: String,
    status: String,
    ball: String,
    ball_since: Option<String>,
    summary: String,
    next_action: Option<String>,
    deadlines: String,
    decisions: String,
    open_questions: String,
    truncated: bool,
    source_closed: bool,
    model: Option<String>,
    created_at: String,
}

fn json_list(s: &str) -> serde_json::Value {
    match serde_json::from_str::<serde_json::Value>(s) {
        Ok(v @ serde_json::Value::Array(_)) => v,
        _ => serde_json::Value::Array(Vec::new()),
    }
}

impl From<DigestRow> for Digest {
    fn from(r: DigestRow) -> Self {
        Digest {
            deadlines: json_list(&r.deadlines),
            decisions: json_list(&r.decisions),
            open_questions: json_list(&r.open_questions),
            id: r.id,
            watch_id: r.watch_id,
            content_hash: r.content_hash,
            status: r.status,
            ball: r.ball,
            ball_since: r.ball_since,
            summary: r.summary,
            next_action: r.next_action,
            truncated: r.truncated,
            source_closed: r.source_closed,
            model: r.model,
            created_at: r.created_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WatchItem {
    #[serde(flatten)]
    pub watch: Watch,
    /// Newest digest, if any.
    pub digest: Option<Digest>,
    /// BR-08 / BR-12 (`health`, `finished`, `overdue`, `nextDue`…).
    #[serde(flatten)]
    pub health: WatchHealth,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPulse {
    pub project_id: String,
    pub project_name: String,
    /// BR-09 / BR-10 (`health`, `nextMilestone`, `counts`).
    #[serde(flatten)]
    pub health: ProjectHealth,
    /// BR-08 order: red, yellow, green / pending, done.
    pub watches: Vec<WatchItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchOutcome {
    pub watch: Watch,
    /// The source was already watched (active) in this project: nothing changed.
    pub already_watching: bool,
}

pub struct WatchRequest<'a> {
    pub project_id: &'a str,
    pub source: &'a Source,
    pub via: &'a str,
    pub run_id: Option<&'a str>,
    /// Move an active watch of another project here (BR-01).
    pub move_project: bool,
}

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn watch_not_found(id: &str) -> CodedError {
    CodedError::new("not_found", format!("watch not found: {id}"))
}

/// `(id, name)` of the project named by `input` (id, or name matched
/// case-insensitively), else of `fallback` (BR-03).
pub async fn resolve_project(
    db: &Db,
    input: Option<&str>,
    fallback: Option<&str>,
) -> Result<(String, String), CodedError> {
    let input = input.map(str::trim).filter(|s| !s.is_empty());
    let fallback = fallback.map(str::trim).filter(|s| !s.is_empty());
    let Some(wanted) = input.or(fallback) else {
        return Err(CodedError::new(
            "project_required",
            "choose a project for this source",
        ));
    };
    let by_id: Option<(String, String)> =
        sqlx::query_as("SELECT id, name FROM projects WHERE id = ?")
            .bind(wanted)
            .fetch_optional(db)
            .await
            .map_err(internal)?;
    if let Some(p) = by_id {
        return Ok(p);
    }
    if input.is_none() {
        // The session / context project no longer exists.
        return Err(CodedError::new(
            "project_not_found",
            format!("project not found: {wanted}"),
        ));
    }
    let mut by_name: Vec<(String, String)> =
        sqlx::query_as("SELECT id, name FROM projects WHERE lower(trim(name)) = lower(?) LIMIT 2")
            .bind(wanted)
            .fetch_all(db)
            .await
            .map_err(internal)?;
    match by_name.len() {
        0 => Err(CodedError::new(
            "project_not_found",
            format!("project not found: {wanted}"),
        )),
        1 => Ok(by_name.remove(0)),
        _ => Err(CodedError::new(
            "project_ambiguous",
            format!("several projects are named {wanted}; use the project id"),
        )),
    }
}

/// `owner/repo` of the project's GitHub repos (for a bare `#<n>`).
pub async fn project_github_repos(db: &Db, project_id: &str) -> Result<Vec<String>, CodedError> {
    sqlx::query_scalar(
        "SELECT DISTINCT lower(github_owner || '/' || github_repo) FROM repos \
         WHERE project_id = ? AND provider = 'github' \
           AND COALESCE(github_owner, '') <> '' AND COALESCE(github_repo, '') <> ''",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .map_err(internal)
}

/// BR-02 with DB context: a Devdy Slack thread id resolves to its link, a
/// bare `#<n>` to the project's single GitHub repo.
pub async fn normalize(db: &Db, input: &str, project_id: &str) -> Result<Source, CodedError> {
    let repos = project_github_repos(db, project_id).await?;
    match parse_source(input, &repos) {
        Ok(s) => Ok(s),
        Err(SourceError::Invalid(detail)) => {
            let id = input.trim();
            if is_safe_id(id) {
                let url: Option<Option<String>> =
                    sqlx::query_scalar("SELECT thread_url FROM slack_threads WHERE id = ?")
                        .bind(id)
                        .fetch_optional(db)
                        .await
                        .map_err(internal)?;
                if let Some(url) = url.flatten() {
                    return parse_source(&url, &[]).map_err(Into::into);
                }
            }
            Err(SourceError::Invalid(detail).into())
        }
        Err(e) => Err(e.into()),
    }
}

pub async fn get(db: &Db, id: &str) -> Result<Option<Watch>, CodedError> {
    sqlx::query_as(&format!(
        "SELECT {WATCH_COLS} FROM {WATCH_FROM} WHERE w.id = ?"
    ))
    .bind(id)
    .fetch_optional(db)
    .await
    .map_err(internal)
}

/// The watch of a source (active or not).
pub async fn find_by_source(
    db: &Db,
    source_type: &str,
    source_ref: &str,
) -> Result<Option<Watch>, CodedError> {
    sqlx::query_as(&format!(
        "SELECT {WATCH_COLS} FROM {WATCH_FROM} WHERE w.source_type = ? AND w.source_ref = ?"
    ))
    .bind(source_type)
    .bind(source_ref)
    .fetch_optional(db)
    .await
    .map_err(internal)
}

/// Active watch of a source, if any.
pub async fn is_watched(
    db: &Db,
    source_type: &str,
    source_ref: &str,
) -> Result<Option<Watch>, CodedError> {
    Ok(find_by_source(db, source_type, source_ref)
        .await?
        .filter(|w| w.active))
}

/// Attempts of a write transaction that keeps hitting `SQLITE_BUSY` (another
/// process — e.g. the MCP sidecar — holds the write lock past busy_timeout).
const BUSY_ATTEMPTS: u32 = 5;

fn is_busy(e: &sqlx::Error) -> bool {
    match e {
        sqlx::Error::Database(d) => {
            // SQLITE_BUSY (5) and its extended codes (BUSY_RECOVERY 261, BUSY_SNAPSHOT 517…).
            d.code()
                .and_then(|c| c.parse::<i32>().ok())
                .is_some_and(|c| c & 0xff == 5)
                || d.message().contains("database is locked")
        }
        _ => false,
    }
}

/// Retry `attempt` (one `BEGIN IMMEDIATE` transaction, see `begin_write`)
/// with backoff while the database is busy.
async fn with_busy_retry<T, F, Fut>(mut attempt: F) -> Result<T, CodedError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, TxError>>,
{
    let mut n = 0;
    loop {
        n += 1;
        match attempt().await {
            Err(TxError::Sql(e)) if is_busy(&e) && n < BUSY_ATTEMPTS => {
                tokio::time::sleep(std::time::Duration::from_millis(50 << n)).await;
            }
            Err(TxError::Sql(e)) => return Err(internal(e)),
            Err(TxError::Coded(e)) => return Err(e),
            Ok(v) => return Ok(v),
        }
    }
}

/// The write lock is taken up front, so the read-check-write of the watch
/// rules cannot interleave with another connection or process.
async fn begin_write(db: &Db) -> Result<sqlx::Transaction<'static, sqlx::Sqlite>, sqlx::Error> {
    db.begin_with("BEGIN IMMEDIATE").await
}

/// Error inside a write transaction: SQL errors may be retried, rule errors may not.
enum TxError {
    Sql(sqlx::Error),
    Coded(CodedError),
}

impl From<sqlx::Error> for TxError {
    fn from(e: sqlx::Error) -> Self {
        TxError::Sql(e)
    }
}

async fn find_in(
    conn: &mut sqlx::SqliteConnection,
    where_sql: &str,
    a: &str,
    b: Option<&str>,
) -> Result<Option<Watch>, sqlx::Error> {
    let sql = format!("SELECT {WATCH_COLS} FROM {WATCH_FROM} WHERE {where_sql}");
    let mut q = sqlx::query_as::<_, Watch>(&sql).bind(a);
    if let Some(b) = b {
        q = q.bind(b);
    }
    q.fetch_optional(conn).await
}

/// Re-activate / move a watch; it goes back to `pending` so the app resolves
/// it again (e.g. re-links the Slack thread to the new project). Conditional
/// on the row still being what the caller read.
async fn reactivate_in(
    conn: &mut sqlx::SqliteConnection,
    w: &Watch,
    project_id: &str,
) -> Result<bool, sqlx::Error> {
    let r = sqlx::query(
        "UPDATE pulse_watches SET project_id = ?, active = 1, resolve_status = 'pending', \
         updated_at = ? WHERE id = ? AND project_id = ? AND active = ?",
    )
    .bind(project_id)
    .bind(now())
    .bind(&w.id)
    .bind(&w.project_id)
    .bind(w.active)
    .execute(conn)
    .await?;
    Ok(r.rows_affected() == 1)
}

fn changed_concurrently() -> TxError {
    TxError::Coded(internal("watch changed concurrently; try again"))
}

/// BR-01: one watch per source app-wide.
/// - same project, active → `already_watching`, nothing changes;
/// - same project, inactive → re-activated;
/// - other project, active → `watched_in_other_project` unless `move_project`;
/// - other project, inactive → taken over by this project (not watched anywhere).
///
/// The check and the write run in one `BEGIN IMMEDIATE` transaction, so two
/// writers (app and MCP sidecar, or two sessions) are serialised.
pub async fn watch(db: &Db, req: WatchRequest<'_>) -> Result<WatchOutcome, CodedError> {
    if !VIAS.contains(&req.via) {
        return Err(CodedError::new(
            "invalid_via",
            format!("unknown added_via: {}", req.via),
        ));
    }
    with_busy_retry(|| watch_once(db, &req)).await
}

async fn watch_once(db: &Db, req: &WatchRequest<'_>) -> Result<WatchOutcome, TxError> {
    let mut tx = begin_write(db).await?;
    let src = req.source;
    let ty = src.source_type.as_str();
    let existing = find_in(
        &mut tx,
        "w.source_type = ? AND w.source_ref = ?",
        ty,
        Some(&src.source_ref),
    )
    .await?;
    let id = match existing {
        Some(w) => {
            if w.project_id == req.project_id {
                if w.active {
                    return Ok(WatchOutcome {
                        watch: w,
                        already_watching: true,
                    });
                }
            } else if w.active && !req.move_project {
                return Err(TxError::Coded(CodedError::new(
                    "watched_in_other_project",
                    format!(
                        "this source is already watched in project {}",
                        w.project_name
                    ),
                )));
            }
            if !reactivate_in(&mut tx, &w, req.project_id).await? {
                return Err(changed_concurrently());
            }
            w.id
        }
        None => {
            let id = Uuid::new_v4().to_string();
            let ts = now();
            sqlx::query(
                "INSERT INTO pulse_watches (id, project_id, source_type, source_ref, source_url, \
                 active, added_via, added_run_id, resolve_status, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, 1, ?, ?, 'pending', ?, ?)",
            )
            .bind(&id)
            .bind(req.project_id)
            .bind(ty)
            .bind(&src.source_ref)
            .bind(&src.source_url)
            .bind(req.via)
            .bind(req.run_id)
            .bind(&ts)
            .bind(&ts)
            .execute(&mut *tx)
            .await?;
            id
        }
    };
    let watch = find_in(&mut tx, "w.id = ?", &id, None)
        .await?
        .ok_or_else(changed_concurrently)?;
    tx.commit().await?;
    Ok(WatchOutcome {
        watch,
        already_watching: false,
    })
}

/// BR-04: `active = 0`, digests and the Slack thread are kept.
pub async fn unwatch(db: &Db, id: &str) -> Result<(), CodedError> {
    let r = sqlx::query("UPDATE pulse_watches SET active = 0, updated_at = ? WHERE id = ?")
        .bind(now())
        .bind(id)
        .execute(db)
        .await
        .map_err(internal)?;
    if r.rows_affected() == 0 {
        return Err(watch_not_found(id));
    }
    Ok(())
}

/// "Move to project": keeps the digests, re-resolves the source.
pub async fn move_watch(db: &Db, id: &str, project_id: &str) -> Result<Watch, CodedError> {
    with_busy_retry(|| async {
        let mut tx = begin_write(db).await?;
        let w = find_in(&mut tx, "w.id = ?", id, None)
            .await?
            .ok_or_else(|| TxError::Coded(watch_not_found(id)))?;
        if !reactivate_in(&mut tx, &w, project_id).await? {
            return Err(changed_concurrently());
        }
        let w = find_in(&mut tx, "w.id = ?", id, None)
            .await?
            .ok_or_else(changed_concurrently)?;
        tx.commit().await?;
        Ok(w)
    })
    .await
}

/// Active watches (+ newest digest, health) grouped per project, cards in
/// BR-09 order. `project_ids` keeps only those projects and lists them even
/// without watches.
pub async fn overview(
    db: &Db,
    project_ids: Option<&[String]>,
) -> Result<Vec<ProjectPulse>, CodedError> {
    overview_at(db, project_ids, chrono::Local::now().fixed_offset()).await
}

pub async fn overview_at(
    db: &Db,
    project_ids: Option<&[String]>,
    now: chrono::DateTime<chrono::FixedOffset>,
) -> Result<Vec<ProjectPulse>, CodedError> {
    let watches: Vec<Watch> = sqlx::query_as(&format!(
        "SELECT {WATCH_COLS} FROM {WATCH_FROM} WHERE w.active = 1 \
         ORDER BY p.name COLLATE NOCASE, w.created_at, w.id"
    ))
    .fetch_all(db)
    .await
    .map_err(internal)?;
    // Every kept digest (≤ 5 per watch), newest first: the first is shown,
    // all of them feed BR-12's "done for 7 days".
    let digests: Vec<DigestRow> = sqlx::query_as(
        "SELECT d.id, d.watch_id, d.content_hash, d.status, d.ball, d.ball_since, d.summary, \
         d.next_action, d.deadlines, d.decisions, d.open_questions, d.truncated, \
         d.source_closed, d.model, d.created_at \
         FROM pulse_digests d JOIN pulse_watches w ON w.id = d.watch_id \
         WHERE w.active = 1 ORDER BY d.watch_id, d.created_at DESC, d.id DESC",
    )
    .fetch_all(db)
    .await
    .map_err(internal)?;
    let mut history: HashMap<String, Vec<Digest>> = HashMap::new();
    for d in digests {
        history
            .entry(d.watch_id.clone())
            .or_default()
            .push(Digest::from(d));
    }

    let mut groups: Vec<(String, String, Vec<WatchItem>)> = Vec::new();
    if let Some(ids) = project_ids {
        for id in ids {
            let name: Option<String> = sqlx::query_scalar("SELECT name FROM projects WHERE id = ?")
                .bind(id)
                .fetch_optional(db)
                .await
                .map_err(internal)?;
            if let Some(name) = name {
                if !groups.iter().any(|g| &g.0 == id) {
                    groups.push((id.clone(), name, Vec::new()));
                }
            }
        }
    }
    for w in watches {
        let idx = match groups.iter().position(|g| g.0 == w.project_id) {
            Some(i) => i,
            None if project_ids.is_some() => continue,
            None => {
                groups.push((w.project_id.clone(), w.project_name.clone(), Vec::new()));
                groups.len() - 1
            }
        };
        let hist = history.remove(&w.id).unwrap_or_default();
        let facts = WatchFacts {
            last_error: w.last_error.clone(),
            history: hist.iter().map(digest_facts).collect(),
        };
        let health = health::watch_health(&facts, now);
        let digest = hist.into_iter().next();
        groups[idx].2.push(WatchItem {
            watch: w,
            digest,
            health,
        });
    }
    let mut out: Vec<ProjectPulse> = groups
        .into_iter()
        .map(|(project_id, project_name, mut watches)| {
            watches.sort_by(|a, b| health::watch_order(&a.health, &b.health));
            let hs: Vec<WatchHealth> = watches.iter().map(|w| w.health.clone()).collect();
            ProjectPulse {
                project_id,
                project_name,
                health: health::project_health(&hs),
                watches,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        health::project_order((&a.health, &a.project_name), (&b.health, &b.project_name))
    });
    Ok(out)
}

fn digest_facts(d: &Digest) -> DigestFacts {
    DigestFacts {
        status: d.status.clone(),
        ball: d.ball.clone(),
        ball_since: d.ball_since.clone(),
        deadlines: serde_json::from_value::<Vec<DueItem>>(d.deadlines.clone()).unwrap_or_default(),
        source_closed: d.source_closed,
        created_at: d.created_at.clone(),
    }
}

// ---- BR-11 queue ------------------------------------------------------------

/// Active watches waiting to be resolved.
pub async fn pending_watches(db: &Db) -> Result<Vec<Watch>, CodedError> {
    sqlx::query_as(&format!(
        "SELECT {WATCH_COLS} FROM {WATCH_FROM} \
         WHERE w.resolve_status = 'pending' AND w.active = 1 ORDER BY w.created_at"
    ))
    .fetch_all(db)
    .await
    .map_err(internal)
}

#[derive(Debug, Clone, sqlx::FromRow, PartialEq, Eq)]
pub struct PulseRequest {
    pub id: String,
    pub kind: String,
    pub watch_id: Option<String>,
    pub project_id: Option<String>,
}

pub async fn open_requests(db: &Db) -> Result<Vec<PulseRequest>, CodedError> {
    sqlx::query_as(
        "SELECT id, kind, watch_id, project_id FROM pulse_requests \
         WHERE done_at IS NULL ORDER BY created_at",
    )
    .fetch_all(db)
    .await
    .map_err(internal)
}

pub async fn mark_request_done(db: &Db, id: &str) -> Result<(), CodedError> {
    sqlx::query("UPDATE pulse_requests SET done_at = ? WHERE id = ? AND done_at IS NULL")
        .bind(now())
        .bind(id)
        .execute(db)
        .await
        .map_err(internal)?;
    Ok(())
}

/// Newest `updated_at` of any watch (None when there are none).
pub async fn max_updated_at(db: &Db) -> Result<Option<String>, CodedError> {
    sqlx::query_scalar("SELECT MAX(updated_at) FROM pulse_watches")
        .fetch_one(db)
        .await
        .map_err(internal)
}

/// The poller only runs while a session is running (or the Pulse screen is open).
pub async fn any_run_running(db: &Db) -> Result<bool, CodedError> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM runs WHERE status = 'running')")
        .fetch_one(db)
        .await
        .map_err(internal)
}

// ---- Refresh pipeline (T3) ---------------------------------------------------

/// Digests kept per watch (§5.2).
pub const DIGESTS_KEPT: i64 = 5;

/// A stored Slack thread ready for BR-05 / BR-07.
pub struct SlackThreadData {
    pub title: String,
    pub msgs: Vec<SlackMsg>,
    pub names: Names,
    /// The token user of the thread's workspace (`slack_identities`).
    pub me: Option<String>,
    pub me_name: Option<String>,
}

pub async fn slack_thread_exists(db: &Db, id: &str) -> Result<bool, CodedError> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM slack_threads WHERE id = ?)")
        .bind(id)
        .fetch_one(db)
        .await
        .map_err(internal)
}

pub async fn load_slack_thread(db: &Db, id: &str) -> Result<Option<SlackThreadData>, CodedError> {
    let head: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT title, team_id FROM slack_threads WHERE id = ?")
            .bind(id)
            .fetch_optional(db)
            .await
            .map_err(internal)?;
    let Some((title, team_id)) = head else {
        return Ok(None);
    };
    let mut all_names = crate::slack_threads::store::load_names(db)
        .await
        .map_err(internal)?;
    let names = team_id
        .as_deref()
        .and_then(|t| all_names.remove(t))
        .unwrap_or_default();
    let me: Option<String> = match team_id.as_deref() {
        Some(t) => sqlx::query_scalar("SELECT user_id FROM slack_identities WHERE team_id = ?")
            .bind(t)
            .fetch_optional(db)
            .await
            .map_err(internal)?,
        None => None,
    };
    type Row = (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
        Option<String>,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT ts, user_id, bot_id, author_name, subtype, text_raw, text_vi \
         FROM slack_messages WHERE thread_id = ? ORDER BY seq ASC, ts ASC",
    )
    .bind(id)
    .fetch_all(db)
    .await
    .map_err(internal)?;
    let msgs = rows
        .into_iter()
        .map(
            |(ts, user_id, bot_id, author_name, subtype, text_raw, text_vi)| {
                let author = user_id
                    .as_deref()
                    .and_then(|u| names.user(u))
                    .map(str::to_string)
                    .or(author_name)
                    .or_else(|| user_id.clone())
                    .or_else(|| bot_id.clone())
                    .unwrap_or_else(|| "unknown".to_string());
                SlackMsg {
                    ts,
                    user_id,
                    bot_id,
                    subtype,
                    author,
                    text_raw,
                    text_vi,
                }
            },
        )
        .collect();
    let me_name = me
        .as_deref()
        .and_then(|u| names.user(u))
        .map(str::to_string);
    Ok(Some(SlackThreadData {
        title,
        msgs,
        names,
        me,
        me_name,
    }))
}

pub async fn latest_digest_hash(db: &Db, watch_id: &str) -> Result<Option<String>, CodedError> {
    sqlx::query_scalar(
        "SELECT content_hash FROM pulse_digests WHERE watch_id = ? \
         ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(watch_id)
    .fetch_optional(db)
    .await
    .map_err(internal)
}

/// Store a digest and keep only the newest `DIGESTS_KEPT` of the watch.
pub async fn insert_digest(
    db: &Db,
    watch_id: &str,
    content_hash: &str,
    d: &DigestDraft,
    truncated: bool,
    source_closed: bool,
    model: Option<&str>,
) -> Result<String, CodedError> {
    let id = Uuid::new_v4().to_string();
    let mut tx = db.begin().await.map_err(internal)?;
    sqlx::query(
        "INSERT INTO pulse_digests (id, watch_id, content_hash, status, ball, ball_since, \
         summary, next_action, deadlines, decisions, open_questions, truncated, source_closed, \
         model, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(watch_id)
    .bind(content_hash)
    .bind(&d.status)
    .bind(&d.ball)
    .bind(&d.ball_since)
    .bind(&d.summary)
    .bind(&d.next_action)
    .bind(json(&d.deadlines))
    .bind(json(&d.decisions))
    .bind(json(&d.open_questions))
    .bind(truncated)
    .bind(source_closed)
    .bind(model)
    .bind(now())
    .execute(&mut *tx)
    .await
    .map_err(internal)?;
    sqlx::query(
        "DELETE FROM pulse_digests WHERE watch_id = ?1 AND id NOT IN \
         (SELECT id FROM pulse_digests WHERE watch_id = ?1 \
          ORDER BY created_at DESC, id DESC LIMIT ?2)",
    )
    .bind(watch_id)
    .bind(DIGESTS_KEPT)
    .execute(&mut *tx)
    .await
    .map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    Ok(id)
}

fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "[]".to_string())
}

/// End of one refresh of a watch.
#[derive(Debug, Default)]
pub struct Finish<'a> {
    pub title: Option<&'a str>,
    pub slack_thread_id: Option<&'a str>,
    /// Source fetched (changed or not): `checked_at = now`.
    pub checked: bool,
    /// New `last_error` (None clears it).
    pub error: Option<&'a str>,
    /// New `resolve_status`. While resolving it only applies to a watch that
    /// is still `pending` in the same project (a move re-queues it); otherwise
    /// only to a non-pending watch.
    pub resolve_status: Option<&'a str>,
    pub resolving: bool,
}

/// Leaves `updated_at` alone: it signals edits of the watch itself (BR-11),
/// and the app emits `pulse:changed` for its own refreshes.
pub async fn finish_refresh(db: &Db, w: &Watch, f: Finish<'_>) -> Result<(), CodedError> {
    let ts = now();
    sqlx::query(
        "UPDATE pulse_watches SET title = COALESCE(?1, title), \
         slack_thread_id = COALESCE(?2, slack_thread_id), \
         checked_at = CASE WHEN ?3 THEN ?4 ELSE checked_at END, last_error = ?5, \
         resolve_status = CASE WHEN ?6 IS NOT NULL AND project_id = ?7 AND \
           (CASE WHEN ?8 THEN resolve_status = 'pending' ELSE resolve_status <> 'pending' END) \
           THEN ?6 ELSE resolve_status END \
         WHERE id = ?9",
    )
    .bind(f.title)
    .bind(f.slack_thread_id)
    .bind(f.checked)
    .bind(&ts)
    .bind(f.error)
    .bind(f.resolve_status)
    .bind(&w.project_id)
    .bind(f.resolving)
    .bind(&w.id)
    .execute(db)
    .await
    .map_err(internal)?;
    Ok(())
}

/// §9: a transferred issue follows its new repo unless another watch has it.
pub async fn update_source(db: &Db, w: &Watch, source: &Source) -> Result<bool, CodedError> {
    let r = sqlx::query(
        "UPDATE pulse_watches SET source_ref = ?1, source_url = ?2, updated_at = ?3 \
         WHERE id = ?4 AND NOT EXISTS (SELECT 1 FROM pulse_watches \
           WHERE source_type = ?5 AND source_ref = ?1 AND id <> ?4)",
    )
    .bind(&source.source_ref)
    .bind(&source.source_url)
    .bind(now())
    .bind(&w.id)
    .bind(source.source_type.as_str())
    .execute(db)
    .await
    .map_err(internal)?;
    Ok(r.rows_affected() == 1)
}

/// Active watches in a refresh scope (both None = all).
pub async fn scope_watches(
    db: &Db,
    project_id: Option<&str>,
    watch_id: Option<&str>,
) -> Result<Vec<Watch>, CodedError> {
    sqlx::query_as(&format!(
        "SELECT {WATCH_COLS} FROM {WATCH_FROM} WHERE w.active = 1 \
         AND (?1 IS NULL OR w.project_id = ?1) AND (?2 IS NULL OR w.id = ?2) \
         ORDER BY w.created_at, w.id"
    ))
    .bind(project_id)
    .bind(watch_id)
    .fetch_all(db)
    .await
    .map_err(internal)
}
