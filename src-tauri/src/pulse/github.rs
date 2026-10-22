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

/// Comments are fetched 100 per page until exhausted; this only guards
/// against a misbehaving API (the content cap keeps header + newest anyway).
const MAX_COMMENT_PAGES: u32 = 1_000;
/// Pages of `projectItems` / `fieldValues` looked through on the board query.
const MAX_BOARD_PAGES: usize = 50;

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

const FIELD_VALUE_NODES: &str = "nodes{ __typename ... on ProjectV2ItemFieldDateValue{ date field{ ... on ProjectV2FieldCommon{ id } } } ... on ProjectV2ItemFieldSingleSelectValue{ name field{ ... on ProjectV2FieldCommon{ id } } } ... on ProjectV2ItemFieldIterationValue{ startDate duration field{ ... on ProjectV2FieldCommon{ id } } } }";

/// One page of the issue's board items (each with its first field values).
pub fn board_items_query() -> String {
    format!(
        "query($owner:String!,$repo:String!,$number:Int!,$cursor:String){{ repository(owner:$owner,name:$repo){{ issue(number:$number){{ projectItems(first:50, after:$cursor){{ pageInfo{{ hasNextPage endCursor }} nodes{{ id project{{ id }} fieldValues(first:50){{ pageInfo{{ hasNextPage endCursor }} {FIELD_VALUE_NODES} }} }} }} }} }} }}"
    )
}

/// Further pages of one board item's field values.
pub fn field_values_query() -> String {
    format!(
        "query($id:ID!,$cursor:String){{ node(id:$id){{ ... on ProjectV2Item{{ fieldValues(first:50, after:$cursor){{ pageInfo{{ hasNextPage endCursor }} {FIELD_VALUE_NODES} }} }} }} }}"
    )
}

/// `(nodes, next cursor)` of a GraphQL connection.
fn page(conn: Option<&Value>) -> (Vec<Value>, Option<String>) {
    let Some(conn) = conn else {
        return (Vec::new(), None);
    };
    let nodes = conn
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let next = conn
        .pointer("/pageInfo/hasNextPage")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        .then(|| {
            conn.pointer("/pageInfo/endCursor")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .flatten();
    (nodes, next)
}

/// The item of `board_id` in one `board_items_query` page, else the cursor of
/// the next page.
pub fn find_board_item(resp: &Value, board_id: &str) -> (Option<Value>, Option<String>) {
    let (items, next) = page(resp.pointer("/data/repository/issue/projectItems"));
    let item = items
        .into_iter()
        .find(|i| i.pointer("/project/id").and_then(Value::as_str) == Some(board_id));
    (item, next)
}

/// Board fields of a fully paged item (`fieldValues.nodes` = every value).
pub fn board_fields_of(item: &Value, mappings_json: &str) -> BoardFields {
    let m = parse_board_mappings(mappings_json);
    let f = board_item_fields(item, &m, &Default::default());
    BoardFields {
        start: f.start,
        deadline: f.deadline,
        status: f.status,
    }
}

/// The issue's Start / Deadline / Status on the project's board: pages
/// through the issue's board items until the configured board is found, then
/// through that item's field values. `Ok(None)` = not on the board / no board.
async fn fetch_board(
    client: &octocrab::Octocrab,
    owner: &str,
    repo: &str,
    number: u64,
    mappings_json: &str,
) -> Result<Option<BoardFields>, String> {
    let Some(board_id) = parse_board_mappings(mappings_json).board_id else {
        return Ok(None);
    };
    let gql = |payload: Value| async move {
        let resp: Value = client.graphql(&payload).await.map_err(|e| e.to_string())?;
        // Any error makes the answer unreliable: partial data with a null
        // `projectItems` must not read as "not on the board".
        match resp.get("errors") {
            Some(errs) if !errs.is_null() => Err(errs.to_string()),
            _ => Ok(resp),
        }
    };
    let mut cursor: Option<String> = None;
    let mut item = None;
    for _ in 0..MAX_BOARD_PAGES {
        let resp = gql(serde_json::json!({
            "query": board_items_query(),
            "variables": { "owner": owner, "repo": repo, "number": number, "cursor": cursor },
        }))
        .await?;
        let (found, next) = find_board_item(&resp, &board_id);
        if found.is_some() {
            item = found;
            break;
        }
        match next {
            Some(c) => cursor = Some(c),
            None => return Ok(None),
        }
    }
    let Some(mut item) = item else {
        return Ok(None);
    };
    let (mut values, mut next) = page(item.get("fieldValues"));
    let item_id = item.get("id").and_then(Value::as_str).map(str::to_string);
    for _ in 0..MAX_BOARD_PAGES {
        let (Some(c), Some(id)) = (next.take(), item_id.as_deref()) else {
            break;
        };
        let resp = gql(serde_json::json!({
            "query": field_values_query(),
            "variables": { "id": id, "cursor": c },
        }))
        .await?;
        let (more, n) = page(resp.pointer("/data/node/fieldValues"));
        values.extend(more);
        next = n;
    }
    item["fieldValues"] = serde_json::json!({ "nodes": values });
    Ok(Some(board_fields_of(&item, mappings_json)))
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
        // A transferred issue is served (via redirect) from its new repo,
        // under its new number.
        let (owner, repo) = owner_repo_of(issue.html_url.as_str())
            .unwrap_or_else(|| (owner.to_string(), repo.to_string()));
        let number = issue.number;

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
        // Board data is optional: a failing query only drops it, flagged so
        // the refresh does not mistake the gap for a content change.
        let mut board_unavailable = false;
        let board = match mappings.filter(|m| !m.trim().is_empty()) {
            Some(m) => match fetch_board(&client, &owner, &repo, number, &m).await {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(event = "pulse_board_query_failed", error = %e);
                    board_unavailable = true;
                    None
                }
            },
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
            board_unavailable,
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
    fn board_item_is_found_across_pages_and_fields_are_read() {
        let mappings = r#"{"boardId":"PVT_1","startFieldId":"F_S","deadlineFieldId":"F_D","statusFieldId":"F_ST"}"#;
        let page1 = json!({ "data": { "repository": { "issue": { "projectItems": {
            "pageInfo": { "hasNextPage": true, "endCursor": "c1" },
            "nodes": [{ "id": "I_0", "project": { "id": "PVT_other" }, "fieldValues": { "nodes": [] } }]
        } } } } });
        assert_eq!(find_board_item(&page1, "PVT_1"), (None, Some("c1".into())));
        let page2 = json!({ "data": { "repository": { "issue": { "projectItems": {
            "pageInfo": { "hasNextPage": false, "endCursor": null },
            "nodes": [{ "id": "I_1", "project": { "id": "PVT_1" }, "fieldValues": {
                "pageInfo": { "hasNextPage": true, "endCursor": "f1" },
                "nodes": [{ "__typename": "ProjectV2ItemFieldDateValue", "date": "2026-10-20", "field": { "id": "F_S" } }]
            } }]
        } } } } });
        let (item, next) = find_board_item(&page2, "PVT_1");
        assert_eq!(next, None);
        let mut item = item.unwrap();
        let (mut values, cursor) = page(item.get("fieldValues"));
        assert_eq!(cursor.as_deref(), Some("f1"));
        // Second field-values page carries the deadline and status.
        let more = json!({ "data": { "node": { "fieldValues": {
            "pageInfo": { "hasNextPage": false, "endCursor": null },
            "nodes": [
                { "__typename": "ProjectV2ItemFieldDateValue", "date": "2026-10-30", "field": { "id": "F_D" } },
                { "__typename": "ProjectV2ItemFieldSingleSelectValue", "name": "In progress", "field": { "id": "F_ST" } }
            ]
        } } } });
        let (extra, end) = page(more.pointer("/data/node/fieldValues"));
        assert_eq!(end, None);
        values.extend(extra);
        item["fieldValues"] = json!({ "nodes": values });
        assert_eq!(
            board_fields_of(&item, mappings),
            BoardFields {
                start: Some("2026-10-20".into()),
                deadline: Some("2026-10-30".into()),
                status: Some("In progress".into()),
            }
        );
        // Not on the board at all.
        let none = json!({ "data": { "repository": { "issue": { "projectItems": {
            "pageInfo": { "hasNextPage": false }, "nodes": [] } } } } });
        assert_eq!(find_board_item(&none, "PVT_1"), (None, None));
        assert!(board_items_query().contains("after:$cursor"));
        assert!(field_values_query().contains("fieldValues(first:50, after:$cursor)"));
    }
}
