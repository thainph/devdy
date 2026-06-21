use crate::db::Db;
use crate::github;

use serde::Serialize;
use std::fs;
use std::path::Path;
use tauri::State;
use uuid::Uuid;

#[derive(Debug, Serialize, Clone)]
pub struct RunRecord {
    pub id: String,
    pub project_id: String,
    pub repo_id: Option<String>,
    pub run_type: String,
    pub ref_number: Option<i64>,
    pub status: String,
    pub engine: String,
    pub input_path: Option<String>,
    pub output_path: Option<String>,
    pub session_id: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub created_at: String,
    /// Human label for the run. Null for issue/PR runs (UI derives "Issue #N");
    /// set for standalone `session` runs from the first user message.
    pub title: Option<String>,
}

/// Returns true if a GitHub user account should be treated as a bot and
/// therefore excluded from fetched comments. Catches:
/// - accounts GitHub itself marks with `type == "Bot"` (GitHub Apps)
/// - login suffix `[bot]` (e.g. `dependabot[bot]`, `coderabbitai[bot]`)
/// - well-known review bots that post via PAT under a normal user account
fn is_bot_user(login: &str, user_type: &str) -> bool {
    if user_type.eq_ignore_ascii_case("Bot") {
        return true;
    }
    let lower = login.to_ascii_lowercase();
    if lower.ends_with("[bot]") {
        return true;
    }
    matches!(
        lower.as_str(),
        "coderabbitai"
            | "claude"
            | "claude-bot"
            | "github-actions"
            | "dependabot"
            | "renovate"
            | "codecov"
            | "sonarcloud"
            | "greptileai"
            | "sweep-ai"
    )
}

/// Human-readable label for a PR review's state.
fn review_state_label(state: Option<octocrab::models::pulls::ReviewState>) -> &'static str {
    use octocrab::models::pulls::ReviewState::*;
    match state {
        Some(Approved) => "approved these changes",
        Some(ChangesRequested) => "requested changes",
        Some(Commented) => "commented",
        Some(Dismissed) => "dismissed their review",
        Some(Pending) => "pending review",
        Some(Open) => "review",
        None => "review",
        _ => "review",
    }
}

#[tauri::command]
pub async fn fetch_issue(
    db: State<'_, Db>,
    project_id: String,
    repo_id: String,
    issue_number: u64,
) -> Result<RunRecord, String> {
    use sqlx::Row;

    let project_row = sqlx::query("SELECT path FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    let project_path: String = project_row.get("path");

    let repo_row = sqlx::query("SELECT github_owner, github_repo FROM repos WHERE id = ?")
        .bind(&repo_id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    let owner: Option<String> = repo_row.get("github_owner");
    let repo: Option<String> = repo_row.get("github_repo");

    let owner = owner.ok_or("Repo has no GitHub owner configured")?;
    let repo = repo.ok_or("Repo has no GitHub repo configured")?;

    let client = github::client_for_project(db.inner(), &project_id).await.map_err(|e| e.to_string())?;

    // Fetch issue
    let issue = client
        .issues(&owner, &repo)
        .get(issue_number)
        .await
        .map_err(|e| e.to_string())?;

    // Fetch comments
    let comments = client
        .issues(&owner, &repo)
        .list_comments(issue_number)
        .per_page(100)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    // Build markdown content
    let mut md = format!(
        "---\nissue: {}\ntitle: {}\nauthor: {}\ncreated: {}\nlabels: {}\n---\n\n# {}\n\n{}\n\n",
        issue_number,
        issue.title.as_str(),
        issue.user.login.as_str(),
        issue.created_at.to_string(),
        issue.labels.iter().map(|l| l.name.clone()).collect::<Vec<_>>().join(", "),
        issue.title.as_str(),
        issue.body.as_deref().unwrap_or(""),
    );

    for comment in &comments.items {
        if is_bot_user(comment.user.login.as_str(), comment.user.r#type.as_str()) {
            continue;
        }
        md.push_str(&format!(
            "---\n\n**Comment by {}** ({})\n\n{}\n\n",
            comment.user.login.as_str(),
            comment.created_at.to_string(),
            comment.body.as_deref().unwrap_or(""),
        ));
    }

    // Write file: .devdy/tasks/issue-<n>/issue.md
    let task_dir = Path::new(&project_path)
        .join(".devdy")
        .join("tasks")
        .join(format!("issue-{}", issue_number));
    fs::create_dir_all(&task_dir).map_err(|e| e.to_string())?;
    let file_path = task_dir.join("issue.md");
    fs::write(&file_path, &md).map_err(|e| e.to_string())?;

    // Get default engine
    let engine_row = sqlx::query("SELECT default_engine FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    let engine: String = engine_row.get("default_engine");

    // Insert run record
    let run_id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let file_path_str = file_path.to_string_lossy().to_string();
    sqlx::query(
        "INSERT INTO runs (id, project_id, repo_id, type, ref_number, status, engine, input_path, output_path, created_at) VALUES (?, ?, ?, 'analyze_issue', ?, 'fetched', ?, ?, ?, ?)"
    )
    .bind(&run_id)
    .bind(&project_id)
    .bind(&repo_id)
    .bind(issue_number as i64)
    .bind(&engine)
    .bind(&file_path_str)
    .bind(&file_path_str)
    .bind(&now)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(RunRecord {
        id: run_id,
        project_id,
        repo_id: Some(repo_id),
        run_type: "analyze_issue".to_string(),
        ref_number: Some(issue_number as i64),
        status: "fetched".to_string(),
        engine,
        input_path: Some(file_path_str.clone()),
        output_path: Some(file_path_str),
        session_id: None,
        started_at: None,
        finished_at: None,
        created_at: now,
        title: None,
    })
}

/// Query GitHub's GraphQL API for issues linked to a PR via the "Development"
/// section (`closingIssuesReferences`). This covers both keyword-based links
/// (Fixes/Closes/Resolves) and issues linked manually through the PR sidebar.
/// Returns the lowest-numbered linked issue, or `None` if there are none / the
/// query fails.
async fn detect_linked_issue(
    client: &octocrab::Octocrab,
    owner: &str,
    repo: &str,
    pr_number: u64,
) -> Option<u64> {
    let payload = serde_json::json!({
        "query": "query($owner:String!,$repo:String!,$number:Int!){repository(owner:$owner,name:$repo){pullRequest(number:$number){closingIssuesReferences(first:10){nodes{number}}}}}",
        "variables": { "owner": owner, "repo": repo, "number": pr_number as i64 }
    });
    let resp: serde_json::Value = client.graphql(&payload).await.ok()?;
    let nodes = resp.pointer("/data/repository/pullRequest/closingIssuesReferences/nodes")?
        .as_array()?;
    nodes.iter()
        .filter_map(|n| n.get("number").and_then(|v| v.as_u64()))
        .min()
}

#[tauri::command]
pub async fn fetch_pr(
    db: State<'_, Db>,
    project_id: String,
    repo_id: String,
    pr_number: u64,
    linked_issue: Option<u64>,
) -> Result<RunRecord, String> {
    use sqlx::Row;

    let project_row = sqlx::query("SELECT path FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    let project_path: String = project_row.get("path");

    let repo_row = sqlx::query("SELECT github_owner, github_repo FROM repos WHERE id = ?")
        .bind(&repo_id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    let owner: Option<String> = repo_row.get("github_owner");
    let repo: Option<String> = repo_row.get("github_repo");

    let owner = owner.ok_or("Repo has no GitHub owner configured")?;
    let repo = repo.ok_or("Repo has no GitHub repo configured")?;

    let client = github::client_for_project(db.inner(), &project_id).await.map_err(|e| e.to_string())?;

    // Fetch PR
    let pr = client
        .pulls(&owner, &repo)
        .get(pr_number)
        .await
        .map_err(|e| e.to_string())?;

    // Resolve linked issue: explicit param > GitHub "Development" linkage (GraphQL)
    let linked_issue_number = match linked_issue {
        Some(n) => n,
        None => match detect_linked_issue(&client, &owner, &repo, pr_number).await {
            Some(n) => n,
            None => return Err("NO_LINKED_ISSUE".to_string()),
        },
    };

    // Build markdown
    let mut md = format!(
        "---\npr: {}\nlinked_issue: {}\ntitle: {}\nauthor: {}\nbase: {}\nhead: {}\ncreated: {}\n---\n\n# {}\n\n{}\n\n",
        pr_number,
        linked_issue_number,
        pr.title.as_deref().unwrap_or(""),
        pr.user.as_ref().map(|u| u.login.as_str()).unwrap_or("unknown"),
        pr.base.ref_field,
        pr.head.ref_field,
        pr.created_at.map(|d| d.to_string()).unwrap_or_default(),
        pr.title.as_deref().unwrap_or(""),
        pr.body.as_deref().unwrap_or(""),
    );

    // Fetch files changed
    match client
        .pulls(&owner, &repo)
        .list_files(pr_number)
        .await
    {
        Ok(files) => {
            md.push_str("## Files Changed\n\n");
            for file in &files.items {
                md.push_str(&format!(
                    "- `{}` (+{} -{}) [{:?}]\n",
                    file.filename,
                    file.additions,
                    file.deletions,
                    file.status,
                ));
            }
            md.push('\n');

            md.push_str("## Diffs\n\n");
            for file in &files.items {
                if let Some(patch) = &file.patch {
                    md.push_str(&format!("### `{}`\n\n```diff\n{}\n```\n\n", file.filename, patch));
                }
            }
        }
        Err(e) => {
            md.push_str(&format!("## Files Changed\n\n(Error fetching files: {})\n\n", e));
        }
    }

    // Fetch issue (general / conversation) comments — paginated.
    match client
        .issues(&owner, &repo)
        .list_comments(pr_number)
        .per_page(100)
        .send()
        .await
    {
        Ok(first_page) => {
            let all = match client.all_pages(first_page).await {
                Ok(items) => items,
                Err(_) => Vec::new(),
            };
            let human_comments: Vec<_> = all
                .iter()
                .filter(|c| !is_bot_user(c.user.login.as_str(), c.user.r#type.as_str()))
                .collect();
            if !human_comments.is_empty() {
                md.push_str("## Comments\n\n");
                for comment in human_comments {
                    md.push_str(&format!(
                        "**{}** ({}): {}\n\n",
                        comment.user.login.as_str(),
                        comment.created_at.to_string(),
                        comment.body.as_deref().unwrap_or(""),
                    ));
                }
            }
        }
        Err(_) => {}
    }

    // Fetch PR reviews (the "pullrequestreview-*" entries: summary body +
    // APPROVE / REQUEST_CHANGES state) — paginated.
    if let Ok(first_page) = client.pulls(&owner, &repo).list_reviews(pr_number).per_page(100).send().await {
        let reviews = client.all_pages(first_page).await.unwrap_or_default();
        let mut rendered = String::new();
        for review in &reviews {
            // user is Option<Author>; skip bots and accounts we can't resolve.
            let Some(user) = review.user.as_ref() else { continue };
            if is_bot_user(user.login.as_str(), user.r#type.as_str()) {
                continue;
            }
            let body = review.body.as_deref().unwrap_or("").trim();
            let is_decision = matches!(
                review.state,
                Some(octocrab::models::pulls::ReviewState::Approved)
                    | Some(octocrab::models::pulls::ReviewState::ChangesRequested)
                    | Some(octocrab::models::pulls::ReviewState::Dismissed)
            );
            // Skip empty "commented"/"pending" reviews — those are just
            // containers for inline comments, which we render separately below.
            if body.is_empty() && !is_decision {
                continue;
            }
            let when = review
                .submitted_at
                .map(|d| d.to_string())
                .unwrap_or_default();
            rendered.push_str(&format!(
                "**{}** {} ({})\n\n",
                user.login.as_str(),
                review_state_label(review.state),
                when,
            ));
            if !body.is_empty() {
                rendered.push_str(body);
                rendered.push_str("\n\n");
            }
        }
        if !rendered.is_empty() {
            md.push_str("## Reviews\n\n");
            md.push_str(&rendered);
        }
    }

    // Fetch inline review comments (comments anchored to specific lines of the
    // diff) — paginated.
    if let Ok(first_page) = client.pulls(&owner, &repo).list_comments(Some(pr_number)).per_page(100).send().await {
        let comments = client.all_pages(first_page).await.unwrap_or_default();
        let mut rendered = String::new();
        for comment in &comments {
            let login = comment.user.as_ref().map(|u| u.login.as_str()).unwrap_or("unknown");
            let user_type = comment.user.as_ref().map(|u| u.r#type.as_str()).unwrap_or("");
            if is_bot_user(login, user_type) {
                continue;
            }
            let line = comment.line.or(comment.original_line);
            let location = match line {
                Some(n) => format!("`{}:{}`", comment.path, n),
                None => format!("`{}`", comment.path),
            };
            rendered.push_str(&format!(
                "**{}** on {} ({}):\n\n{}\n\n",
                login,
                location,
                comment.created_at.to_string(),
                comment.body.trim(),
            ));
        }
        if !rendered.is_empty() {
            md.push_str("## Inline Review Comments\n\n");
            md.push_str(&rendered);
        }
    }

    // Write file: .devdy/tasks/issue-<linked>/pr-<pr_number>.md
    let task_dir = Path::new(&project_path)
        .join(".devdy")
        .join("tasks")
        .join(format!("issue-{}", linked_issue_number));
    fs::create_dir_all(&task_dir).map_err(|e| e.to_string())?;
    let file_path = task_dir.join(format!("pr-{}.md", pr_number));
    fs::write(&file_path, &md).map_err(|e| e.to_string())?;

    let engine_row = sqlx::query("SELECT default_engine FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    let engine: String = engine_row.get("default_engine");

    let run_id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let file_path_str = file_path.to_string_lossy().to_string();
    sqlx::query(
        "INSERT INTO runs (id, project_id, repo_id, type, ref_number, status, engine, input_path, output_path, created_at) VALUES (?, ?, ?, 'review_pr', ?, 'fetched', ?, ?, ?, ?)"
    )
    .bind(&run_id)
    .bind(&project_id)
    .bind(&repo_id)
    .bind(pr_number as i64)
    .bind(&engine)
    .bind(&file_path_str)
    .bind(&file_path_str)
    .bind(&now)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(RunRecord {
        id: run_id,
        project_id,
        repo_id: Some(repo_id),
        run_type: "review_pr".to_string(),
        ref_number: Some(pr_number as i64),
        status: "fetched".to_string(),
        engine,
        input_path: Some(file_path_str.clone()),
        output_path: Some(file_path_str),
        session_id: None,
        started_at: None,
        finished_at: None,
        created_at: now,
        title: None,
    })
}

#[tauri::command]
pub async fn list_runs(
    db: State<'_, Db>,
    project_id: String,
) -> Result<Vec<RunRecord>, String> {
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT id, project_id, repo_id, type, ref_number, status, engine, input_path, output_path, session_id, started_at, finished_at, created_at, title
         FROM runs WHERE project_id = ? ORDER BY created_at DESC LIMIT 50"
    )
    .bind(&project_id)
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows.iter().map(|row| RunRecord {
        id: row.get("id"),
        project_id: row.get("project_id"),
        repo_id: row.get("repo_id"),
        run_type: row.get("type"),
        ref_number: row.get("ref_number"),
        status: row.get("status"),
        engine: row.get("engine"),
        input_path: row.get("input_path"),
        output_path: row.get("output_path"),
        session_id: row.get("session_id"),
        started_at: row.get("started_at"),
        finished_at: row.get("finished_at"),
        created_at: row.get("created_at"),
        title: row.get("title"),
    }).collect())
}
