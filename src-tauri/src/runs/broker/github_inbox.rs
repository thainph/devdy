//! GitHub inbox requests from the built-in `devdy` MCP server
//! (`sidecar-mcp/lib/github-inbox-tools.mjs`). Read-only.
//!
//! The PR Inbox (`pr_inbox`) and GitHub Mentions (`github_mentions`) lists only
//! live in the app's memory and are fetched with the user's GitHub accounts, so
//! the MCP server asks for them here and gets the same list the windows show —
//! never a token. Like the rest of the broker it is fail-closed: only a live
//! run of the claimed project may ask.
//!
//! - `github_prs`: the review-requested PRs (`CachedPrInbox`).
//! - `github_mentions`: the direct mentions (`CachedMentions`).
//!
//! `argv = ["refresh"]` refetches now; otherwise the cached list is returned
//! unless it is missing or older than [`MAX_AGE`] (the pollers' default).

use std::time::Duration;

use super::{audit, ApproverResolver, BrokerRequest, BrokerResponse};
use crate::db::Db;
use crate::{github_mentions, pr_inbox};
use tauri::AppHandle;

pub const PRS: &str = "github_prs";
pub const MENTIONS: &str = "github_mentions";

/// A cached list older than this is refetched first.
const MAX_AGE: Duration = Duration::from_secs(5 * 60);

pub fn handles(tool: &str) -> bool {
    matches!(tool, PRS | MENTIONS)
}

fn fresh(age: Option<Duration>, refresh: bool) -> bool {
    !refresh && age.is_some_and(|a| a < MAX_AGE)
}

pub async fn process(
    req: &BrokerRequest,
    db: &Db,
    resolver: &dyn ApproverResolver,
    app: Option<&AppHandle>,
) -> BrokerResponse {
    let run_id = req.run_id.as_deref().unwrap_or("?");
    let tool = req.tool.as_str();
    let deny = |reason: &str| {
        audit::audit_request(run_id, tool, &req.argv, "deny", Some(reason));
        BrokerResponse::deny(reason)
    };
    match req.run_id.as_deref().and_then(|r| resolver.run_project(r)) {
        None => return deny("no live run for this request"),
        Some(p) if p != req.project_id => return deny("run does not belong to this project"),
        Some(_) => {}
    }
    let Some(app) = app else { return deny("app not available") };
    let refresh = req.argv.iter().any(|a| a == "refresh");

    let data = if tool == PRS {
        // A failed refetch still returns the cached list, with the error.
        let error = if fresh(pr_inbox::cache_age(), refresh) {
            None
        } else {
            pr_inbox::refresh(app, db, false).await.err()
        };
        match pr_inbox::get_cached_pr_inbox() {
            Some(c) => serde_json::to_value(c).map(|v| (v, error)),
            None => return deny(&error.unwrap_or_else(|| "PR inbox not loaded".into())),
        }
    } else {
        let error = if fresh(github_mentions::cache_age(), refresh) {
            None
        } else {
            github_mentions::refresh(app, db).await.err()
        };
        match github_mentions::get_cached_github_mentions() {
            Some(c) => serde_json::to_value(c).map(|v| (v, error)),
            None => return deny(&error.unwrap_or_else(|| "mentions not loaded".into())),
        }
    };
    let (data, error) = match data {
        Ok(d) => d,
        Err(e) => return deny(&e.to_string()),
    };
    audit::audit_request(run_id, tool, &req.argv, "allow", None);
    BrokerResponse {
        decision: "allow".into(),
        reason: error,
        data: Some(data),
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

    fn req(json: &str) -> BrokerRequest {
        serde_json::from_str(json).unwrap()
    }

    #[tokio::test]
    async fn only_a_live_run_of_the_claimed_project_may_read() {
        let db = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        for tool in [PRS, MENTIONS] {
            for json in [
                format!(r#"{{"project_id":"p2","run_id":"r1","tool":"{tool}","argv":[]}}"#),
                format!(r#"{{"project_id":"p1","run_id":"r9","tool":"{tool}","argv":[]}}"#),
                format!(r#"{{"project_id":"p1","tool":"{tool}","argv":[]}}"#),
            ] {
                let resp = process(&req(&json), &db, &Live, None).await;
                assert_eq!(resp.decision, "deny");
                assert!(resp.data.is_none() && resp.token.is_none());
            }
            // A live run but no app (unit tests, headless): nothing to read from.
            let json = format!(r#"{{"project_id":"p1","run_id":"r1","tool":"{tool}","argv":[]}}"#);
            assert_eq!(process(&req(&json), &db, &Live, None).await.decision, "deny");
        }
    }

    #[test]
    fn a_cached_list_is_reused_only_while_fresh_and_not_forced() {
        assert!(fresh(Some(Duration::from_secs(60)), false));
        assert!(!fresh(Some(Duration::from_secs(60)), true));
        assert!(!fresh(Some(MAX_AGE), false));
        assert!(!fresh(None, false));
    }
}
