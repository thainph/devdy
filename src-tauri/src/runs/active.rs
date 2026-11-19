//! The runs whose sidecar is alive, with the metadata the Work Dashboard shows
//! (`list_active_runs`): name, project, engine / model, start time, status and
//! whether the run is blocked on a permission prompt. Spans every project,
//! unlike the per-project run lists.

use crate::db::Db;
use serde::Serialize;
use sqlx::Row;
use std::collections::HashMap;

#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct ActiveRun {
    pub run_id: String,
    pub project_id: String,
    pub project_name: String,
    pub title: Option<String>,
    pub run_type: String,
    pub ref_number: Option<i64>,
    pub engine: Option<String>,
    pub model: Option<String>,
    pub status: String,
    /// Start of the current turn (falls back to the creation time).
    pub started_at: Option<String>,
    pub created_at: String,
    pub role: Option<String>,
    pub conductor_run_id: Option<String>,
    /// Open permission prompts the run is waiting on.
    pub pending_permissions: usize,
    /// When the oldest open prompt arrived (RFC3339).
    pub waiting_since: Option<String>,
    /// The run's sidecar is alive. False only for a conductor listed because
    /// its workers still run while it sits between turns (see
    /// [`active_runs`]).
    pub live: bool,
    /// Label of its Claude account (see `run_history::ACCOUNT_AND_REPO`).
    pub account_name: Option<String>,
    /// `github` | `gitlab` for an issue / PR run with a repo.
    pub repo_provider: Option<String>,
    /// `owner/repo` (GitHub) or the project path (GitLab).
    pub repo_path: Option<String>,
    pub pinned: bool,
    pub protected: bool,
}

/// Open permission prompts of a run: (count, oldest arrival).
pub type PendingInfo = (usize, Option<String>);

/// Rows of `run_ids` (live ids missing from the DB are skipped), with their
/// pending prompts. The conductor of a live worker is listed too even while
/// its own sidecar is down between turns (it is resumed when a worker
/// finishes), so its workers keep their group instead of flipping to
/// standalone; such a conductor carries `live: false`. Ordered like the
/// dashboard lists them: awaiting a permission first (longest waiting first),
/// then the longest running.
pub async fn active_runs(
    db: &Db,
    run_ids: &[String],
    pending: &HashMap<String, PendingInfo>,
) -> Result<Vec<ActiveRun>, sqlx::Error> {
    let mut out = fetch_rows(db, run_ids, pending).await?;
    let mut missing: Vec<String> = out
        .iter()
        .filter_map(|r| r.conductor_run_id.clone())
        .filter(|c| !out.iter().any(|r| &r.run_id == c))
        .collect();
    missing.sort();
    missing.dedup();
    let mut parents = fetch_rows(db, &missing, pending).await?;
    for p in &mut parents {
        p.live = false;
    }
    out.extend(parents);
    out.sort_by(|a, b| {
        (b.pending_permissions > 0)
            .cmp(&(a.pending_permissions > 0))
            .then_with(|| a.waiting_since.cmp(&b.waiting_since))
            .then_with(|| a.started_at.cmp(&b.started_at))
            .then_with(|| a.run_id.cmp(&b.run_id))
    });
    Ok(out)
}

async fn fetch_rows(
    db: &Db,
    run_ids: &[String],
    pending: &HashMap<String, PendingInfo>,
) -> Result<Vec<ActiveRun>, sqlx::Error> {
    if run_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; run_ids.len()].join(", ");
    let sql = format!(
        "SELECT r.id, r.project_id, p.name AS project_name, r.title, r.type, r.ref_number, r.engine, \
                r.model, r.status, COALESCE(r.started_at, r.created_at) AS started_at, r.created_at, \
                r.role, r.conductor_run_id, r.pinned, r.protected, {account_and_repo} \
         FROM runs r JOIN projects p ON p.id = r.project_id {repo_join} \
         WHERE r.id IN ({placeholders})",
        account_and_repo = super::run_history::ACCOUNT_AND_REPO,
        repo_join = super::run_history::REPO_JOIN,
    );
    let mut q = sqlx::query(&sql);
    for id in run_ids {
        q = q.bind(id);
    }
    let rows = q.fetch_all(db).await?;
    Ok(rows
        .iter()
        .map(|row| {
            let run_id: String = row.get("id");
            let (pending_permissions, waiting_since) =
                pending.get(&run_id).cloned().unwrap_or_default();
            ActiveRun {
                project_id: row.get("project_id"),
                project_name: row.get("project_name"),
                title: row.get("title"),
                run_type: row.get("type"),
                ref_number: row.get("ref_number"),
                engine: row.get("engine"),
                model: row.get("model"),
                status: row.get("status"),
                started_at: row.get("started_at"),
                created_at: row.get("created_at"),
                role: row.get("role"),
                conductor_run_id: row.get("conductor_run_id"),
                pending_permissions,
                waiting_since,
                live: true,
                account_name: row.get("account_name"),
                repo_provider: row.get("repo_provider"),
                repo_path: row.get::<Option<String>, _>("repo_path").filter(|s| !s.trim().is_empty()),
                pinned: row.get::<i64, _>("pinned") != 0,
                protected: row.get::<i64, _>("protected") != 0,
                run_id,
            }
        })
        .collect())
}

/// Pending prompts of every run, from the permission store.
pub fn pending_by_run() -> HashMap<String, PendingInfo> {
    let mut out: HashMap<String, PendingInfo> = HashMap::new();
    for p in super::permission::all_pending() {
        let entry = out.entry(p.event.run_id.clone()).or_insert((0, None));
        entry.0 += 1;
        let at = p.requested_at.to_rfc3339();
        if entry.1.as_ref().is_none_or(|cur| at < *cur) {
            entry.1 = Some(at);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slack_threads::tests::memory_db;

    async fn run(db: &Db, id: &str, project: &str, started: &str) {
        sqlx::query(
            "INSERT INTO runs (id, project_id, type, status, engine, model, created_at, started_at, title) \
             VALUES (?, ?, 'session', 'running', 'claude', 'opus', '2026-01-01T00:00:00Z', ?, ?)",
        )
        .bind(id)
        .bind(project)
        .bind(started)
        .bind(format!("Run {id}"))
        .execute(db)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn lists_live_runs_across_projects_waiting_first() {
        let db = memory_db().await;
        run(&db, "a", "p1", "2026-01-02T10:00:00Z").await;
        run(&db, "b", "p2", "2026-01-02T08:00:00Z").await;
        run(&db, "c", "p1", "2026-01-02T09:00:00Z").await;
        run(&db, "dead", "p1", "2026-01-01T00:00:00Z").await;

        let mut pending = HashMap::new();
        pending.insert(
            "a".to_string(),
            (2, Some("2026-01-02T10:05:00Z".to_string())),
        );
        let ids: Vec<String> = ["a", "b", "c", "ghost"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        let rows = active_runs(&db, &ids, &pending).await.unwrap();
        let order: Vec<&str> = rows.iter().map(|r| r.run_id.as_str()).collect();
        assert_eq!(order, ["a", "b", "c"]);
        assert_eq!(rows[0].pending_permissions, 2);
        assert_eq!(rows[0].project_name, "proj");
        assert_eq!(rows[1].project_name, "proj2");
        assert_eq!(rows[1].engine.as_deref(), Some("claude"));
        assert_eq!(rows[1].title.as_deref(), Some("Run b"));

        assert!(active_runs(&db, &[], &pending).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn lists_the_conductor_of_a_live_worker() {
        let db = memory_db().await;
        run(&db, "cond", "p1", "2026-01-02T08:00:00Z").await;
        run(&db, "w1", "p1", "2026-01-02T09:00:00Z").await;
        run(&db, "w2", "p1", "2026-01-02T10:00:00Z").await;
        sqlx::query("UPDATE runs SET role = 'conductor', status = 'done' WHERE id = 'cond'")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("UPDATE runs SET role = 'worker', conductor_run_id = 'cond' WHERE id IN ('w1', 'w2')")
            .execute(&db)
            .await
            .unwrap();

        let ids: Vec<String> = ["w1", "w2"].iter().map(|s| s.to_string()).collect();
        let rows = active_runs(&db, &ids, &HashMap::new()).await.unwrap();
        let order: Vec<&str> = rows.iter().map(|r| r.run_id.as_str()).collect();
        assert_eq!(order, ["cond", "w1", "w2"]);
        assert_eq!(rows[0].role.as_deref(), Some("conductor"));
        assert!(!rows[0].live);
        assert!(rows[1..].iter().all(|r| r.live));
    }

    #[tokio::test]
    async fn carries_account_repo_and_flags() {
        let db = memory_db().await;
        run(&db, "pr", "p1", "2026-01-02T10:00:00Z").await;
        run(&db, "s", "p1", "2026-01-02T11:00:00Z").await;
        for q in [
            "INSERT INTO claude_accounts (id, label, config_dir, is_default, created_at) \
             VALUES ('acc1', 'Work', '/tmp/a1', 1, '2026-01-01')",
            "INSERT INTO repos (id, project_id, name, path, github_owner, github_repo, created_at) \
             VALUES ('r1', 'p1', 'api', '/tmp/api', 'malme', 'api', '2026-01-01')",
            "UPDATE runs SET type = 'review_pr', ref_number = 7, repo_id = 'r1', pinned = 1 WHERE id = 'pr'",
        ] {
            sqlx::query(q).execute(&db).await.unwrap();
        }
        let ids = vec!["pr".to_string(), "s".to_string()];
        let rows = active_runs(&db, &ids, &HashMap::new()).await.unwrap();
        let pr = rows.iter().find(|r| r.run_id == "pr").unwrap();
        assert_eq!(pr.repo_path.as_deref(), Some("malme/api"));
        assert_eq!(pr.account_name.as_deref(), Some("Work"));
        assert!(pr.pinned && !pr.protected);
        assert_eq!(rows.iter().find(|r| r.run_id == "s").unwrap().repo_path, None);
    }
}
