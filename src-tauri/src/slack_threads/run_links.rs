//! Slack thread ↔ session (run) links, the `slack_thread_runs` table: link
//! (idempotent, the first source wins), unlink, the sessions of a thread, the
//! threads of a session, and the clean-ups on run delete / thread merge.
//! Readers join `runs`, so a link whose run is gone is never listed.

use super::store::{self, internal, not_found, SlackThreadSummary};
use super::CodedError;
use crate::db::Db;
use serde::Serialize;
use sqlx::Row;

/// Where a link came from.
pub const SOURCES: [&str; 3] = ["manual", "picker", "created_from_thread"];

/// A session linked to a thread, with what its chip needs.
#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SlackThreadRunLink {
    pub thread_id: String,
    pub run_id: String,
    /// The run's current project.
    pub project_id: String,
    /// `manual` | `picker` | `created_from_thread`
    pub source: String,
    pub linked_at: String,
    pub title: Option<String>,
    /// `session` | `analyze_issue` | `review_pr`
    pub run_type: String,
    pub ref_number: Option<i64>,
    /// `fetched` | `running` | `done` | `failed` | `cancelled`
    pub status: String,
    pub engine: String,
    pub last_activity_at: Option<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LinkRunResult {
    /// false = the link already existed (kept as is).
    pub created: bool,
    /// The thread had no project and took the run's.
    pub project_set: bool,
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Link a thread to a run. Re-linking is a no-op (the first source and time
/// are kept). A thread without a project takes the run's.
pub async fn link(
    db: &Db,
    thread_id: &str,
    run_id: &str,
    source: &str,
) -> Result<LinkRunResult, CodedError> {
    if !SOURCES.contains(&source) {
        return Err(CodedError::new(
            "invalid_source",
            format!("unknown link source: {source}"),
        ));
    }
    let thread: Option<Option<String>> =
        sqlx::query_scalar("SELECT project_id FROM slack_threads WHERE id = ?")
            .bind(thread_id)
            .fetch_optional(db)
            .await
            .map_err(internal)?;
    let thread_project = thread.ok_or_else(|| not_found(thread_id))?;
    let run_project: String = sqlx::query_scalar("SELECT project_id FROM runs WHERE id = ?")
        .bind(run_id)
        .fetch_optional(db)
        .await
        .map_err(internal)?
        .ok_or_else(|| CodedError::new("run_not_found", format!("session not found: {run_id}")))?;
    let ts = now();
    let mut tx = db.begin().await.map_err(internal)?;
    let created = sqlx::query(
        "INSERT OR IGNORE INTO slack_thread_runs (thread_id, run_id, project_id, source, linked_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(thread_id)
    .bind(run_id)
    .bind(&run_project)
    .bind(source)
    .bind(&ts)
    .execute(&mut *tx)
    .await
    .map_err(internal)?
    .rows_affected()
        > 0;
    let project_set = thread_project.is_none()
        && sqlx::query(
            "UPDATE slack_threads SET project_id = ?, updated_at = ? \
             WHERE id = ? AND project_id IS NULL",
        )
        .bind(&run_project)
        .bind(&ts)
        .bind(thread_id)
        .execute(&mut *tx)
        .await
        .map_err(internal)?
        .rows_affected()
            > 0;
    tx.commit().await.map_err(internal)?;
    Ok(LinkRunResult {
        created,
        project_set,
    })
}

/// Remove one link; `false` when there was none.
pub async fn unlink(db: &Db, thread_id: &str, run_id: &str) -> Result<bool, CodedError> {
    let r = sqlx::query("DELETE FROM slack_thread_runs WHERE thread_id = ? AND run_id = ?")
        .bind(thread_id)
        .bind(run_id)
        .execute(db)
        .await
        .map_err(internal)?;
    Ok(r.rows_affected() > 0)
}

/// Sessions linked to a thread whose run still exists, most recently active
/// first.
pub async fn runs_of_thread(
    db: &Db,
    thread_id: &str,
) -> Result<Vec<SlackThreadRunLink>, CodedError> {
    let rows = sqlx::query(
        "SELECT l.thread_id, l.run_id, l.source, l.linked_at, r.project_id, r.title, r.type, \
         r.ref_number, r.status, r.engine, \
         COALESCE(r.last_activity_at, r.finished_at, r.started_at, r.created_at) AS activity \
         FROM slack_thread_runs l JOIN runs r ON r.id = l.run_id \
         WHERE l.thread_id = ? \
         ORDER BY activity DESC, l.linked_at DESC",
    )
    .bind(thread_id)
    .fetch_all(db)
    .await
    .map_err(internal)?;
    Ok(rows
        .iter()
        .map(|row| SlackThreadRunLink {
            thread_id: row.get("thread_id"),
            run_id: row.get("run_id"),
            project_id: row.get("project_id"),
            source: row.get("source"),
            linked_at: row.get("linked_at"),
            title: row.get("title"),
            run_type: row.get("type"),
            ref_number: row.get("ref_number"),
            status: row.get("status"),
            engine: row.get("engine"),
            last_activity_at: row.get("activity"),
        })
        .collect())
}

/// Threads linked to a run, most recently linked first.
pub async fn threads_of_run(db: &Db, run_id: &str) -> Result<Vec<SlackThreadSummary>, CodedError> {
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT l.thread_id FROM slack_thread_runs l \
         JOIN slack_threads t ON t.id = l.thread_id \
         WHERE l.run_id = ? ORDER BY l.linked_at DESC",
    )
    .bind(run_id)
    .fetch_all(db)
    .await
    .map_err(internal)?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        out.push(store::get_summary(db, &id).await?);
    }
    Ok(out)
}

/// Drop every link of a deleted run; returns the threads that lost one.
pub async fn delete_for_run(db: &Db, run_id: &str) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar("DELETE FROM slack_thread_runs WHERE run_id = ? RETURNING thread_id")
        .bind(run_id)
        .fetch_all(db)
        .await
}

/// A merged-away thread hands its links to the kept one (existing links of
/// the kept thread win).
pub async fn move_links(db: &Db, from: &str, into: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT OR IGNORE INTO slack_thread_runs (thread_id, run_id, project_id, source, linked_at) \
         SELECT ?, run_id, project_id, source, linked_at FROM slack_thread_runs WHERE thread_id = ?",
    )
    .bind(into)
    .bind(from)
    .execute(db)
    .await?;
    sqlx::query("DELETE FROM slack_thread_runs WHERE thread_id = ?")
        .bind(from)
        .execute(db)
        .await?;
    Ok(())
}

/// Drop every link of a project's runs (call BEFORE the project row goes: its
/// runs cascade away with it). Returns how many links were removed.
pub async fn delete_for_project(db: &Db, project_id: &str) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        "DELETE FROM slack_thread_runs WHERE project_id = ?1 \
         OR run_id IN (SELECT id FROM runs WHERE project_id = ?1)",
    )
    .bind(project_id)
    .execute(db)
    .await?
    .rows_affected())
}
