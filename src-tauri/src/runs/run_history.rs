//! `list_run_history`: the Dashboard's History — every session that ran, in
//! every project, latest activity first, one page at a time (the list loads
//! older pages as it scrolls). A conductor's workers are left out: their
//! conductor stands for them and reports how many it has.
//!
//! "Unread" is the persisted read state the Remote Control Inbox already uses
//! (`runs.seen_at`, set by `mark_run_seen`): a run whose last turn finished
//! after the user last saw it.

use crate::db::Db;
use serde::Serialize;
use sqlx::Row;
use tauri::State;

/// Rows per page when the caller does not say.
const DEFAULT_LIMIT: u32 = 40;
const MAX_LIMIT: u32 = 200;

const ACTIVITY: &str = "COALESCE(r.last_activity_at, r.finished_at, r.started_at, r.created_at)";
const UNREAD: &str = "(r.status != 'running' AND r.finished_at IS NOT NULL \
     AND (r.seen_at IS NULL OR r.seen_at < r.finished_at))";

#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRun {
    pub run_id: String,
    pub project_id: String,
    pub project_name: String,
    /// `session` | `analyze_issue` | `review_pr` | …
    pub run_type: String,
    pub ref_number: Option<i64>,
    pub title: Option<String>,
    /// Issue / PR title, when the run is about one.
    pub ref_title: Option<String>,
    pub status: String,
    pub engine: String,
    pub role: Option<String>,
    /// Workers this run spawned (a conductor), 0 otherwise.
    pub worker_count: i64,
    pub pinned: bool,
    pub unread: bool,
    /// RFC3339-ish, as stored; the sort key (with `run_id` as tie-break).
    pub last_activity_at: String,
}

/// Escape `%`, `_` and `\` for a `LIKE … ESCAPE '\'` pattern.
fn like_pattern(q: &str) -> String {
    let mut out = String::with_capacity(q.len() + 2);
    out.push('%');
    for c in q.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// One page of History. `before` is the last row of the previous page
/// (`last_activity_at`, `run_id`); None starts from the newest.
pub async fn list_run_history_inner(
    db: &Db,
    query: Option<&str>,
    unread_only: bool,
    before: Option<(&str, &str)>,
    limit: u32,
) -> Result<Vec<HistoryRun>, String> {
    let limit = if limit == 0 { DEFAULT_LIMIT } else { limit.min(MAX_LIMIT) };
    let query = query.map(str::trim).filter(|q| !q.is_empty());
    let mut sql = format!(
        "SELECT r.id, r.project_id, p.name AS project_name, r.type, r.ref_number, r.title, r.ref_title, \
                r.status, r.engine, r.role, r.pinned, {ACTIVITY} AS activity, {UNREAD} AS unread, \
                (SELECT COUNT(*) FROM runs w WHERE w.conductor_run_id = r.id) AS worker_count \
         FROM runs r JOIN projects p ON p.id = r.project_id \
         WHERE r.conductor_run_id IS NULL AND r.status != 'fetched'"
    );
    if query.is_some() {
        sql.push_str(" AND (r.title LIKE ? ESCAPE '\\' OR r.ref_title LIKE ? ESCAPE '\\')");
    }
    if unread_only {
        sql.push_str(&format!(" AND {UNREAD}"));
    }
    if before.is_some() {
        sql.push_str(&format!(" AND ({ACTIVITY} < ? OR ({ACTIVITY} = ? AND r.id < ?))"));
    }
    sql.push_str(&format!(" ORDER BY activity DESC, r.id DESC LIMIT {limit}"));

    let mut q = sqlx::query(&sql);
    if let Some(text) = query {
        let pat = like_pattern(text);
        q = q.bind(pat.clone()).bind(pat);
    }
    if let Some((at, id)) = before {
        q = q.bind(at.to_string()).bind(at.to_string()).bind(id.to_string());
    }
    let rows = q.fetch_all(db).await.map_err(|e| e.to_string())?;
    Ok(rows
        .iter()
        .map(|row| HistoryRun {
            run_id: row.get("id"),
            project_id: row.get("project_id"),
            project_name: row.get("project_name"),
            run_type: row.get("type"),
            ref_number: row.get("ref_number"),
            title: row.get::<Option<String>, _>("title").filter(|s| !s.trim().is_empty()),
            ref_title: row.get::<Option<String>, _>("ref_title").filter(|s| !s.is_empty()),
            status: row.get("status"),
            engine: row.get("engine"),
            role: row.get("role"),
            worker_count: row.get("worker_count"),
            pinned: row.get::<i64, _>("pinned") != 0,
            unread: row.get::<i64, _>("unread") != 0,
            last_activity_at: row.get("activity"),
        })
        .collect())
}

#[tauri::command]
pub async fn list_run_history(
    db: State<'_, Db>,
    query: Option<String>,
    unread_only: Option<bool>,
    before_at: Option<String>,
    before_id: Option<String>,
    limit: Option<u32>,
) -> Result<Vec<HistoryRun>, String> {
    let before = before_at.as_deref().zip(before_id.as_deref());
    list_run_history_inner(
        db.inner(),
        query.as_deref(),
        unread_only.unwrap_or(false),
        before,
        limit.unwrap_or(DEFAULT_LIMIT),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slack_threads::tests::memory_db;

    async fn insert(db: &Db, id: &str, project: &str, status: &str, activity: &str, finished: Option<&str>, seen: Option<&str>) {
        sqlx::query(
            "INSERT INTO runs (id, project_id, type, status, engine, created_at, started_at, finished_at, last_activity_at, seen_at, title) \
             VALUES (?, ?, 'session', ?, 'claude', '2026-11-01T00:00:00+00:00', '2026-11-01T00:00:01+00:00', ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(project)
        .bind(status)
        .bind(finished)
        .bind(activity)
        .bind(seen)
        .bind(format!("Run {id}"))
        .execute(db)
        .await
        .unwrap();
    }

    fn ids(rows: &[HistoryRun]) -> Vec<&str> {
        rows.iter().map(|r| r.run_id.as_str()).collect()
    }

    #[tokio::test]
    async fn lists_every_project_latest_first_and_pages() {
        let db = memory_db().await;
        insert(&db, "a", "p1", "done", "2026-11-10T01:00:00+00:00", Some("2026-11-10T01:00:00+00:00"), Some("2026-11-10T02:00:00+00:00")).await;
        insert(&db, "b", "p2", "done", "2026-11-12T01:00:00+00:00", Some("2026-11-12T01:00:00+00:00"), None).await;
        insert(&db, "c", "p1", "running", "2026-11-13T01:00:00+00:00", None, None).await;
        insert(&db, "d", "p2", "failed", "2026-11-11T01:00:00+00:00", Some("2026-11-11T01:00:00+00:00"), Some("2026-11-10T00:00:00+00:00")).await;
        // A worker and a never-started fetched run stay out.
        insert(&db, "w", "p1", "done", "2026-11-14T01:00:00+00:00", Some("2026-11-14T01:00:00+00:00"), None).await;
        sqlx::query("UPDATE runs SET conductor_run_id = 'c' WHERE id = 'w'").execute(&db).await.unwrap();
        insert(&db, "f", "p1", "fetched", "2026-11-15T01:00:00+00:00", None, None).await;

        let all = list_run_history_inner(&db, None, false, None, 0).await.unwrap();
        assert_eq!(ids(&all), ["c", "b", "d", "a"]);
        assert_eq!(all[0].worker_count, 1);
        assert_eq!(all[1].project_name, "proj2");
        // Unread: finished after it was last seen (or never seen); a running run never is.
        let unread: Vec<bool> = all.iter().map(|r| r.unread).collect();
        assert_eq!(unread, [false, true, true, false]);

        let first = list_run_history_inner(&db, None, false, None, 2).await.unwrap();
        assert_eq!(ids(&first), ["c", "b"]);
        let last = first.last().unwrap();
        let next = list_run_history_inner(&db, None, false, Some((&last.last_activity_at, &last.run_id)), 2).await.unwrap();
        assert_eq!(ids(&next), ["d", "a"]);

        let only_unread = list_run_history_inner(&db, None, true, None, 0).await.unwrap();
        assert_eq!(ids(&only_unread), ["b", "d"]);
    }

    #[tokio::test]
    async fn searches_by_name() {
        let db = memory_db().await;
        insert(&db, "x", "p1", "done", "2026-11-10T01:00:00+00:00", Some("2026-11-10T01:00:00+00:00"), None).await;
        insert(&db, "y", "p1", "done", "2026-11-11T01:00:00+00:00", Some("2026-11-11T01:00:00+00:00"), None).await;
        sqlx::query("UPDATE runs SET title = 'Fix 100% CPU' WHERE id = 'x'").execute(&db).await.unwrap();
        sqlx::query("UPDATE runs SET title = NULL, ref_title = 'Broker policy' WHERE id = 'y'").execute(&db).await.unwrap();

        let hit = list_run_history_inner(&db, Some("100%"), false, None, 0).await.unwrap();
        assert_eq!(ids(&hit), ["x"]);
        let by_ref = list_run_history_inner(&db, Some("broker"), false, None, 0).await.unwrap();
        assert_eq!(ids(&by_ref), ["y"]);
        assert_eq!(by_ref[0].title, None);
        assert!(list_run_history_inner(&db, Some("  "), false, None, 0).await.unwrap().len() == 2);
    }
}
