//! SQL for Project Pulse: watch rules (BR-01/03/04), project lookup, the
//! overview read and the BR-11 request queue. Errors are `CodedError`s.

use super::source::{parse_source, Source, SourceError};
use crate::db::Db;
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
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPulse {
    pub project_id: String,
    pub project_name: String,
    /// BR-09 project health; filled in by the health stage (None for now).
    pub health: Option<String>,
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

async fn get_required(db: &Db, id: &str) -> Result<Watch, CodedError> {
    get(db, id).await?.ok_or_else(|| watch_not_found(id))
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

/// Re-activate / move a watch; it goes back to `pending` so the app resolves
/// it again (e.g. re-links the Slack thread to the new project).
async fn reactivate(db: &Db, id: &str, project_id: &str) -> Result<(), CodedError> {
    sqlx::query(
        "UPDATE pulse_watches SET project_id = ?, active = 1, resolve_status = 'pending', \
         updated_at = ? WHERE id = ?",
    )
    .bind(project_id)
    .bind(now())
    .bind(id)
    .execute(db)
    .await
    .map_err(internal)?;
    Ok(())
}

/// BR-01: one watch per source app-wide.
/// - same project, active → `already_watching`, nothing changes;
/// - same project, inactive → re-activated;
/// - other project, active → `watched_in_other_project` unless `move_project`;
/// - other project, inactive → taken over by this project (not watched anywhere).
pub async fn watch(db: &Db, req: WatchRequest<'_>) -> Result<WatchOutcome, CodedError> {
    if !VIAS.contains(&req.via) {
        return Err(CodedError::new(
            "invalid_via",
            format!("unknown added_via: {}", req.via),
        ));
    }
    let src = req.source;
    let ty = src.source_type.as_str();
    // Two passes: an insert racing another writer (e.g. the MCP sidecar) falls
    // back to the existing-row rules.
    for _ in 0..2 {
        if let Some(w) = find_by_source(db, ty, &src.source_ref).await? {
            if w.project_id == req.project_id {
                if w.active {
                    return Ok(WatchOutcome {
                        watch: w,
                        already_watching: true,
                    });
                }
            } else if w.active && !req.move_project {
                return Err(CodedError::new(
                    "watched_in_other_project",
                    format!(
                        "this source is already watched in project {}",
                        w.project_name
                    ),
                ));
            }
            reactivate(db, &w.id, req.project_id).await?;
            return Ok(WatchOutcome {
                watch: get_required(db, &w.id).await?,
                already_watching: false,
            });
        }
        let id = Uuid::new_v4().to_string();
        let ts = now();
        let r = sqlx::query(
            "INSERT INTO pulse_watches (id, project_id, source_type, source_ref, source_url, \
             active, added_via, added_run_id, resolve_status, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, 1, ?, ?, 'pending', ?, ?) \
             ON CONFLICT(source_type, source_ref) DO NOTHING",
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
        .execute(db)
        .await
        .map_err(internal)?;
        if r.rows_affected() == 1 {
            return Ok(WatchOutcome {
                watch: get_required(db, &id).await?,
                already_watching: false,
            });
        }
    }
    Err(internal("pulse watch insert kept conflicting"))
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
    get_required(db, id).await?;
    reactivate(db, id, project_id).await?;
    get_required(db, id).await
}

/// Active watches (+ newest digest) grouped per project. `project_ids` keeps
/// only those projects and lists them even without watches.
pub async fn overview(
    db: &Db,
    project_ids: Option<&[String]>,
) -> Result<Vec<ProjectPulse>, CodedError> {
    let watches: Vec<Watch> = sqlx::query_as(&format!(
        "SELECT {WATCH_COLS} FROM {WATCH_FROM} WHERE w.active = 1 \
         ORDER BY p.name COLLATE NOCASE, w.created_at, w.id"
    ))
    .fetch_all(db)
    .await
    .map_err(internal)?;
    let digests: Vec<DigestRow> = sqlx::query_as(
        "SELECT d.id, d.watch_id, d.content_hash, d.status, d.ball, d.ball_since, d.summary, \
         d.next_action, d.deadlines, d.decisions, d.open_questions, d.truncated, d.model, \
         d.created_at FROM pulse_digests d JOIN pulse_watches w ON w.id = d.watch_id \
         WHERE w.active = 1 AND d.id = (SELECT d2.id FROM pulse_digests d2 \
           WHERE d2.watch_id = d.watch_id ORDER BY d2.created_at DESC, d2.id DESC LIMIT 1)",
    )
    .fetch_all(db)
    .await
    .map_err(internal)?;
    let mut latest: HashMap<String, Digest> = digests
        .into_iter()
        .map(|d| (d.watch_id.clone(), Digest::from(d)))
        .collect();

    let mut out: Vec<ProjectPulse> = Vec::new();
    if let Some(ids) = project_ids {
        for id in ids {
            let name: Option<String> = sqlx::query_scalar("SELECT name FROM projects WHERE id = ?")
                .bind(id)
                .fetch_optional(db)
                .await
                .map_err(internal)?;
            if let Some(name) = name {
                if !out.iter().any(|p| &p.project_id == id) {
                    out.push(ProjectPulse {
                        project_id: id.clone(),
                        project_name: name,
                        health: None,
                        watches: Vec::new(),
                    });
                }
            }
        }
    }
    for w in watches {
        let idx = match out.iter().position(|p| p.project_id == w.project_id) {
            Some(i) => i,
            None if project_ids.is_some() => continue,
            None => {
                out.push(ProjectPulse {
                    project_id: w.project_id.clone(),
                    project_name: w.project_name.clone(),
                    health: None,
                    watches: Vec::new(),
                });
                out.len() - 1
            }
        };
        let digest = latest.remove(&w.id);
        out[idx].watches.push(WatchItem { watch: w, digest });
    }
    Ok(out)
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

/// Resolve result of a watch. Only applies while the watch is still `pending`
/// with the same project (a concurrent move re-queues it).
pub async fn set_resolved(
    db: &Db,
    w: &Watch,
    slack_thread_id: Option<&str>,
    title: Option<&str>,
    error: Option<&str>,
) -> Result<bool, CodedError> {
    let status = if error.is_some() { "error" } else { "ready" };
    let r = sqlx::query(
        "UPDATE pulse_watches SET resolve_status = ?, last_error = ?, \
         slack_thread_id = COALESCE(?, slack_thread_id), title = COALESCE(?, title), \
         updated_at = ? WHERE id = ? AND resolve_status = 'pending' AND project_id = ?",
    )
    .bind(status)
    .bind(error)
    .bind(slack_thread_id)
    .bind(title)
    .bind(now())
    .bind(&w.id)
    .bind(&w.project_id)
    .execute(db)
    .await
    .map_err(internal)?;
    Ok(r.rows_affected() == 1)
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

/// The poller only runs while a session is running (or the Pulse screen is open).
pub async fn any_run_running(db: &Db) -> Result<bool, CodedError> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM runs WHERE status = 'running')")
        .fetch_one(db)
        .await
        .map_err(internal)
}
