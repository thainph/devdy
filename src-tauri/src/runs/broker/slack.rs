//! Slack thread requests from the built-in `devdy` MCP server
//! (`sidecar-mcp/lib/slack-thread-tools.mjs`).
//!
//! The MCP server reads Slack threads from SQLite but has no Slack token, so
//! adding a thread, refreshing it and posting a reply run here through
//! `SlackThreadsState` (the same paths the UI uses) and only the outcome goes
//! back — never a token. Fail-closed like the rest of the broker: only a live
//! run of the claimed project may ask.
//!
//! - `slack_add` (`argv = [url, "global"?]`): add / re-import by link, linked to
//!   the run's project unless `global`. The fetch + translate job is queued.
//! - `slack_refresh` (`argv = [thread_id]`): queue a refetch.
//! - `slack_reply` (`argv = [thread_id]`, `text`): post a reply AS THE USER.
//!   Always asks the user first, whatever the run's permission mode, showing
//!   the thread and the exact text that will be posted.

use super::approver::ActionPrompt;
use super::{audit, ApproverResolver, BrokerRequest, BrokerResponse};
use crate::db::Db;
use crate::slack_threads::{store, SlackThreadsState, MAX_REPLY_CHARS};
use tauri::{AppHandle, Manager};

pub const ADD: &str = "slack_add";
pub const REFRESH: &str = "slack_refresh";
pub const REPLY: &str = "slack_reply";

pub fn handles(tool: &str) -> bool {
    matches!(tool, ADD | REFRESH | REPLY)
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
    let project = match req.run_id.as_deref().and_then(|r| resolver.run_project(r)) {
        None => return deny("no live run for this request"),
        Some(p) if p != req.project_id => return deny("run does not belong to this project"),
        Some(p) => p,
    };
    let Some(target) = req.argv.first().map(|s| s.trim()).filter(|s| !s.is_empty()) else {
        return deny(if tool == ADD { "missing thread link" } else { "missing thread id" });
    };

    // Reply: validate and get the user's go-ahead before touching the app state.
    if tool == REPLY {
        let text = req.text.as_deref().map(str::trim).unwrap_or("");
        if text.is_empty() {
            return deny("reply text is empty");
        }
        if text.chars().count() > MAX_REPLY_CHARS {
            return deny(&format!("reply exceeds {MAX_REPLY_CHARS} characters"));
        }
        let summary = match store::get_summary(db, target).await {
            Ok(s) => s,
            Err(e) => return deny(&e.to_string()),
        };
        let Some(approver) = resolver.resolve(req.run_id.as_deref()).await else {
            return deny("no live run to approve against");
        };
        let channel = summary.channel.as_deref().map(|c| format!("#{c}"));
        let prompt = ActionPrompt {
            title: "Gửi trả lời lên Slack thread?".into(),
            description: format!(
                "AI muốn đăng tin nhắn dưới tên bạn vào thread “{}”{}.",
                summary.title,
                channel.as_deref().map(|c| format!(" ({c})")).unwrap_or_default(),
            ),
            display_name: "Slack reply".into(),
            input: serde_json::json!({
                "thread": summary.title,
                "channel": channel,
                "workspace": summary.workspace,
                "url": summary.thread_url,
                "text": text,
            }),
        };
        if !approver.approve_action(req, prompt).await {
            return deny("user declined the Slack reply");
        }
    }

    let Some(state) = app.and_then(|a| a.try_state::<SlackThreadsState>()) else {
        return deny("Slack threads are unavailable");
    };
    let result = match tool {
        ADD => {
            let project = (req.argv.get(1).map(String::as_str) != Some("global")).then_some(project);
            state
                .add_thread(target, project.as_deref(), None, false)
                .await
                .map(|r| serde_json::to_value(r).unwrap_or_default())
        }
        REFRESH => state
            .refresh(target)
            .await
            .map(|s| serde_json::json!({ "id": s.id, "fetch_status": s.fetch_status })),
        _ => state
            .post_reply(target, req.text.as_deref().unwrap_or(""))
            .await
            .map(|s| serde_json::json!({ "id": s.id, "fetch_status": s.fetch_status })),
    };
    match result {
        Ok(v) => {
            audit::audit_request(run_id, tool, &req.argv, "allow", None);
            BrokerResponse {
                decision: "allow".into(),
                slack: Some(v),
                ..Default::default()
            }
        }
        Err(e) => deny(&e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runs::broker::approver::{Approver, FixedApprover};
    use async_trait::async_trait;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::sync::Arc;

    struct Live(bool);
    #[async_trait]
    impl ApproverResolver for Live {
        async fn resolve(&self, _run_id: Option<&str>) -> Option<Arc<dyn Approver>> {
            Some(Arc::new(FixedApprover(self.0)))
        }
        fn run_project(&self, run_id: &str) -> Option<String> {
            (run_id == "r1").then(|| "p1".to_string())
        }
    }

    async fn db() -> Db {
        let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    fn req(json: &str) -> BrokerRequest {
        serde_json::from_str(json).unwrap()
    }

    #[tokio::test]
    async fn only_a_live_run_of_the_claimed_project_may_ask() {
        let db = db().await;
        for json in [
            r#"{"project_id":"p2","run_id":"r1","tool":"slack_refresh","argv":["t1"]}"#,
            r#"{"project_id":"p1","run_id":"r9","tool":"slack_add","argv":["https://x.slack.com/archives/C1/p1"]}"#,
            r#"{"project_id":"p1","tool":"slack_reply","argv":["t1"],"text":"hi"}"#,
        ] {
            assert_eq!(process(&req(json), &db, &Live(true), None).await.decision, "deny");
        }
    }

    #[tokio::test]
    async fn reply_is_validated_before_asking() {
        let db = db().await;
        for (json, why) in [
            (r#"{"project_id":"p1","run_id":"r1","tool":"slack_reply","argv":[],"text":"hi"}"#, "missing thread id"),
            (r#"{"project_id":"p1","run_id":"r1","tool":"slack_reply","argv":["t1"],"text":"  "}"#, "reply text is empty"),
            (r#"{"project_id":"p1","run_id":"r1","tool":"slack_reply","argv":["t1"]}"#, "reply text is empty"),
        ] {
            let resp = process(&req(json), &db, &Live(true), None).await;
            assert_eq!((resp.decision.as_str(), resp.reason.as_deref()), ("deny", Some(why)));
        }
        let long = format!(
            r#"{{"project_id":"p1","run_id":"r1","tool":"slack_reply","argv":["t1"],"text":"{}"}}"#,
            "a".repeat(MAX_REPLY_CHARS + 1)
        );
        assert!(process(&req(&long), &db, &Live(true), None).await.reason.unwrap().contains("exceeds"));
        // Unknown thread: denied without prompting.
        let resp = process(
            &req(r#"{"project_id":"p1","run_id":"r1","tool":"slack_reply","argv":["nope"],"text":"hi"}"#),
            &db,
            &Live(true),
            None,
        )
        .await;
        assert_eq!(resp.decision, "deny");
        assert!(resp.slack.is_none());
    }

    #[tokio::test]
    async fn declined_reply_is_denied() {
        let db = db().await;
        sqlx::query(
            "INSERT INTO slack_threads (id, title, thread_url, fetch_status, created_at, updated_at)
             VALUES ('t1', 'Deploy', 'https://x.slack.com/archives/C1/p1700000000000100', 'ready', '', '')",
        )
        .execute(&db)
        .await
        .unwrap();
        let json = r#"{"project_id":"p1","run_id":"r1","tool":"slack_reply","argv":["t1"],"text":"hi"}"#;
        let resp = process(&req(json), &db, &Live(false), None).await;
        assert_eq!(resp.reason.as_deref(), Some("user declined the Slack reply"));
        // Approved, but no app state in tests: still fail-closed.
        let resp = process(&req(json), &db, &Live(true), None).await;
        assert_eq!(resp.reason.as_deref(), Some("Slack threads are unavailable"));
    }
}
