//! GitHub side of the Pulse refresh: issue + comments + state + labels +
//! assignees + milestone over REST with the project's GitHub account, plus the
//! issue's Start / Deadline / Status on the project's board (one GraphQL
//! query on the issue's `projectItems`). Read-only.

use super::content::{BoardFields, IssueComment, IssueData};
use crate::commands::github::{board_item_fields, is_bot_user, parse_board_mappings};
use crate::db::Db;
use crate::slack_threads::CodedError;
use async_trait::async_trait;
use serde_json::Value;

/// Comments are fetched 100 per page, at most this many pages.
const MAX_COMMENT_PAGES: u32 = 10;

#[async_trait]
pub trait GithubFetch: Send + Sync {
    /// The issue as seen with `project_id`'s GitHub account. Errors:
    /// `github_not_found` (404/410), `github_auth` (401/403, no account / PAT),
    /// `github_fetch_failed` (anything else).
    async fn fetch_issue(
        &self,
        project_id: &str,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<IssueData, CodedError>;

    /// Login of the project's GitHub account (prompt context), if known.
    async fn my_login(&self, project_id: &str) -> Option<String>;
}

/// HTTP status → watch error code (§7).
pub fn status_error(status: u16, detail: impl Into<String>) -> CodedError {
    let code = match status {
        404 | 410 => "github_not_found",
        401 | 403 => "github_auth",
        _ => "github_fetch_failed",
    };
    CodedError::new(code, detail)
}

fn octo_error(e: octocrab::Error) -> CodedError {
    match &e {
        octocrab::Error::GitHub { source, .. } => {
            status_error(source.status_code.as_u16(), source.message.clone())
        }
        _ => CodedError::new("github_fetch_failed", e.to_string()),
    }
}

/// `https://github.com/<o>/<r>/issues/<n>` → `(o, r)`.
pub fn owner_repo_of(html_url: &str) -> Option<(String, String)> {
    let rest = html_url.split_once("github.com/")?.1;
    let mut parts = rest.split('/');
    let owner = parts.next().filter(|s| !s.is_empty())?;
    let repo = parts.next().filter(|s| !s.is_empty())?;
    Some((owner.to_string(), repo.to_string()))
}

pub const BOARD_QUERY: &str = "query($owner:String!,$repo:String!,$number:Int!){ repository(owner:$owner,name:$repo){ issue(number:$number){ projectItems(first:20){ nodes{ project{ id } fieldValues(first:30){ nodes{ __typename ... on ProjectV2ItemFieldDateValue{ date field{ ... on ProjectV2FieldCommon{ id } } } ... on ProjectV2ItemFieldSingleSelectValue{ name field{ ... on ProjectV2FieldCommon{ id } } } ... on ProjectV2ItemFieldIterationValue{ startDate duration field{ ... on ProjectV2FieldCommon{ id } } } } } } } } } }";

/// The issue's fields on the board described by `mappings_json`
/// (`projects.github_project_field_mappings`) from a `BOARD_QUERY` response.
pub fn parse_issue_board(resp: &Value, mappings_json: &str) -> Option<BoardFields> {
    let m = parse_board_mappings(mappings_json);
    let board_id = m.board_id.as_deref()?;
    let items = resp
        .pointer("/data/repository/issue/projectItems/nodes")?
        .as_array()?;
    let item = items
        .iter()
        .find(|i| i.pointer("/project/id").and_then(Value::as_str) == Some(board_id))?;
    let f = board_item_fields(item, &m, &Default::default());
    Some(BoardFields {
        start: f.start,
        deadline: f.deadline,
        status: f.status,
    })
}

/// Octocrab with the project's GitHub account (`github::client_for_project`).
pub struct OctoGithub {
    pub db: Db,
}

#[async_trait]
impl GithubFetch for OctoGithub {
    async fn fetch_issue(
        &self,
        project_id: &str,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<IssueData, CodedError> {
        let client = crate::github::client_for_project(&self.db, project_id)
            .await
            .map_err(|e| CodedError::new("github_auth", e.to_string()))?;
        let issues = client.issues(owner, repo);
        let issue = issues.get(number).await.map_err(octo_error)?;
        // A transferred issue is served (via redirect) from its new repo.
        let (owner, repo) = owner_repo_of(issue.html_url.as_str())
            .unwrap_or_else(|| (owner.to_string(), repo.to_string()));

        let mut comments = Vec::new();
        if issue.comments > 0 {
            let issues = client.issues(&owner, &repo);
            for page in 1..=MAX_COMMENT_PAGES {
                let batch = issues
                    .list_comments(number)
                    .per_page(100)
                    .page(page)
                    .send()
                    .await
                    .map_err(octo_error)?;
                let n = batch.items.len();
                for c in batch.items {
                    if is_bot_user(c.user.login.as_str(), c.user.r#type.as_str()) {
                        continue;
                    }
                    comments.push(IssueComment {
                        author: c.user.login.clone(),
                        created_at: c.created_at.to_rfc3339(),
                        body: c.body.unwrap_or_default(),
                    });
                }
                if n < 100 {
                    break;
                }
            }
        }

        let mappings: Option<String> =
            sqlx::query_scalar("SELECT github_project_field_mappings FROM projects WHERE id = ?")
                .bind(project_id)
                .fetch_optional(&self.db)
                .await
                .ok()
                .flatten();
        let board = match mappings.filter(|m| !m.trim().is_empty()) {
            Some(m) => {
                let payload = serde_json::json!({
                    "query": BOARD_QUERY,
                    "variables": { "owner": owner, "repo": repo, "number": number },
                });
                // Board data is optional: a failing query only drops it.
                match client.graphql::<Value>(&payload).await {
                    Ok(resp) => parse_issue_board(&resp, &m),
                    Err(e) => {
                        tracing::warn!(event = "pulse_board_query_failed", error = %e);
                        None
                    }
                }
            }
            None => None,
        };

        let state = serde_json::to_value(&issue.state)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "open".to_string());
        let state_reason = issue
            .state_reason
            .as_ref()
            .and_then(|r| serde_json::to_value(r).ok())
            .and_then(|v| v.as_str().map(str::to_string));
        Ok(IssueData {
            owner,
            repo,
            number,
            title: issue.title.clone(),
            state,
            state_reason,
            labels: issue.labels.iter().map(|l| l.name.clone()).collect(),
            assignees: issue.assignees.iter().map(|a| a.login.clone()).collect(),
            milestone: issue.milestone.as_ref().map(|m| m.title.clone()),
            milestone_due: issue
                .milestone
                .as_ref()
                .and_then(|m| m.due_on)
                .map(|d| d.format("%Y-%m-%d").to_string()),
            author: issue.user.login.clone(),
            created_at: issue.created_at.to_rfc3339(),
            body: issue.body.clone().unwrap_or_default(),
            comments,
            is_pull_request: issue.pull_request.is_some(),
            board,
        })
    }

    async fn my_login(&self, project_id: &str) -> Option<String> {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT a.username FROM projects p JOIN github_accounts a \
             ON a.id = p.github_account_id WHERE p.id = ?",
        )
        .bind(project_id)
        .fetch_optional(&self.db)
        .await
        .ok()
        .flatten()
        .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn status_codes_map_to_watch_errors() {
        assert_eq!(status_error(404, "x").code, "github_not_found");
        assert_eq!(status_error(410, "x").code, "github_not_found");
        assert_eq!(status_error(401, "x").code, "github_auth");
        assert_eq!(status_error(403, "x").code, "github_auth");
        assert_eq!(status_error(500, "x").code, "github_fetch_failed");
    }

    #[test]
    fn transferred_issue_repo() {
        assert_eq!(
            owner_repo_of("https://github.com/NewOrg/new-app/issues/7"),
            Some(("NewOrg".into(), "new-app".into()))
        );
        assert_eq!(owner_repo_of("https://example.com/x"), None);
    }

    #[test]
    fn board_fields_of_the_project_board_only() {
        let mappings = r#"{"boardId":"PVT_1","startFieldId":"F_S","deadlineFieldId":"F_D","statusFieldId":"F_ST"}"#;
        let resp = json!({ "data": { "repository": { "issue": { "projectItems": { "nodes": [
            { "project": { "id": "PVT_other" }, "fieldValues": { "nodes": [
                { "__typename": "ProjectV2ItemFieldDateValue", "date": "2030-01-01", "field": { "id": "F_D" } }
            ] } },
            { "project": { "id": "PVT_1" }, "fieldValues": { "nodes": [
                { "__typename": "ProjectV2ItemFieldDateValue", "date": "2026-10-20", "field": { "id": "F_S" } },
                { "__typename": "ProjectV2ItemFieldDateValue", "date": "2026-10-30", "field": { "id": "F_D" } },
                { "__typename": "ProjectV2ItemFieldSingleSelectValue", "name": "In progress", "field": { "id": "F_ST" } }
            ] } }
        ] } } } } });
        assert_eq!(
            parse_issue_board(&resp, mappings),
            Some(BoardFields {
                start: Some("2026-10-20".into()),
                deadline: Some("2026-10-30".into()),
                status: Some("In progress".into()),
            })
        );
        // Not on the board / no board configured.
        assert_eq!(parse_issue_board(&resp, r#"{"boardId":"PVT_9"}"#), None);
        assert_eq!(parse_issue_board(&resp, "{}"), None);
        assert_eq!(parse_issue_board(&json!({}), mappings), None);
    }
}
