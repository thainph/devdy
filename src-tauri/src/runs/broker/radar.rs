//! `radar` requests: the built-in `devdy` MCP server asking the app to refetch
//! Delivery Radar boards (`sidecar-mcp/lib/radar-store.mjs`).
//!
//! The MCP server reads the radar snapshots from SQLite but has no GitHub
//! credentials, so a refresh is done here with each project's own accounts and
//! only the outcome goes back — never a token. `argv` lists the project ids to
//! refetch (empty = every project the radar covers); ids outside the radar are
//! ignored. Like the rest of the broker it is fail-closed: only a live run of
//! the claimed project may ask. The board data itself is the same the MCP
//! server can already read from the shared store, which spans every project.

use std::collections::HashMap;

use super::{audit, ApproverResolver, BrokerRequest, BrokerResponse};
use crate::commands::radar;
use crate::db::Db;

pub const TOOL: &str = "radar";

pub async fn process(req: &BrokerRequest, db: &Db, resolver: &dyn ApproverResolver) -> BrokerResponse {
    let run_id = req.run_id.as_deref().unwrap_or("?");
    let deny = |reason: &str| {
        audit::audit_request(run_id, TOOL, &req.argv, "deny", Some(reason));
        BrokerResponse::deny(reason)
    };
    match req.run_id.as_deref().and_then(|r| resolver.run_project(r)) {
        None => return deny("no live run for this request"),
        Some(p) if p != req.project_id => return deny("run does not belong to this project"),
        Some(_) => {}
    }

    let covered = match radar::radar_project_ids(db).await {
        Ok(ids) => ids,
        Err(e) => return deny(&e),
    };
    let ids: Vec<String> = if req.argv.is_empty() {
        covered
    } else {
        covered.into_iter().filter(|id| req.argv.contains(id)).collect()
    };
    let errors: HashMap<String, String> = radar::refresh_projects(db, ids).await.into_iter().collect();
    audit::audit_request(run_id, TOOL, &req.argv, "allow", None);
    BrokerResponse {
        decision: "allow".into(),
        errors: (!errors.is_empty()).then_some(errors),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runs::broker::approver::{Approver, FixedApprover};
    use async_trait::async_trait;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::sync::Arc;

    struct Live;
    #[async_trait]
    impl ApproverResolver for Live {
        async fn resolve(&self, _run_id: Option<&str>) -> Option<Arc<dyn Approver>> {
            Some(Arc::new(FixedApprover(true)))
        }
        fn run_project(&self, run_id: &str) -> Option<String> {
            (run_id == "r1").then(|| "p1".to_string())
        }
    }

    async fn db() -> Db {
        let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            "CREATE TABLE repos (project_id TEXT, provider TEXT, github_owner TEXT, github_repo TEXT)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    fn req(json: &str) -> BrokerRequest {
        serde_json::from_str(json).unwrap()
    }

    #[tokio::test]
    async fn only_a_live_run_of_the_claimed_project_may_refresh() {
        let db = db().await;
        for json in [
            r#"{"project_id":"p2","run_id":"r1","tool":"radar","argv":[]}"#,
            r#"{"project_id":"p1","run_id":"r9","tool":"radar","argv":[]}"#,
            r#"{"project_id":"p1","tool":"radar","argv":[]}"#,
        ] {
            assert_eq!(process(&req(json), &db, &Live).await.decision, "deny");
        }
    }

    #[tokio::test]
    async fn nothing_to_refresh_is_allowed() {
        // No GitHub repo anywhere: no project to fetch, no error.
        let db = db().await;
        let resp = process(&req(r#"{"project_id":"p1","run_id":"r1","tool":"radar","argv":[]}"#), &db, &Live).await;
        assert_eq!(resp.decision, "allow");
        assert!(resp.errors.is_none() && resp.token.is_none());
    }
}
