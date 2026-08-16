use crate::db::Db;
use crate::github;
use crate::gitlab;
use crate::secrets;

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
    /// Pinned runs sort to the top of the History list.
    #[serde(default)]
    pub pinned: bool,
}

/// Repo identity loaded from the `repos` row, used to branch fetch by provider
/// (FR-005/BR-001) and to build the `repo_slug` task directory (BR-008).
struct RepoIdentity {
    id: String,
    provider: String,
    github_owner: Option<String>,
    github_repo: Option<String>,
    gitlab_project_path: Option<String>,
    gitlab_project_id: Option<i64>,
}

impl RepoIdentity {
    /// `<owner_or_namespace>`/`<repo>` pair used to build the repo_slug.
    /// For GitHub uses owner/repo; for GitLab splits `namespace/project` from
    /// the stored path (last segment = repo, the rest = namespace).
    fn slug_owner_repo(&self) -> (String, String) {
        match self.provider.as_str() {
            "gitlab" => {
                let path = self.gitlab_project_path.clone().unwrap_or_default();
                match path.rsplit_once('/') {
                    Some((ns, proj)) => (ns.to_string(), proj.to_string()),
                    None => (String::new(), path),
                }
            }
            _ => (
                self.github_owner.clone().unwrap_or_default(),
                self.github_repo.clone().unwrap_or_default(),
            ),
        }
    }

    fn slug(&self) -> String {
        let (owner, repo) = self.slug_owner_repo();
        gitlab::repo_slug(&self.provider, &owner, &repo, &self.id)
    }
}

/// Load a repo's provider identity from the DB.
async fn load_repo_identity(db: &Db, repo_id: &str) -> Result<RepoIdentity, String> {
    use sqlx::Row;
    let row = sqlx::query(
        "SELECT id, provider, github_owner, github_repo, gitlab_project_path, gitlab_project_id FROM repos WHERE id = ?",
    )
    .bind(repo_id)
    .fetch_one(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(RepoIdentity {
        id: row.get("id"),
        // Older rows may predate the column; treat NULL/empty as github (BR-001).
        provider: row
            .get::<Option<String>, _>("provider")
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| "github".to_string()),
        github_owner: row.get("github_owner"),
        github_repo: row.get("github_repo"),
        gitlab_project_path: row.get("gitlab_project_path"),
        gitlab_project_id: row.get("gitlab_project_id"),
    })
}

/// Build a GitLab REST client for a project's linked GitLab account (SEC-001/002).
/// Resolves host from the account and the PAT from the Keychain at call time.
async fn gitlab_client_for(
    db: &Db,
    project_id: &str,
    repo: &RepoIdentity,
) -> Result<gitlab::GitlabClient, String> {
    use sqlx::Row;
    let row = sqlx::query("SELECT gitlab_account_id FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_one(db)
        .await
        .map_err(|e| e.to_string())?;
    let account_id: Option<String> = row.get("gitlab_account_id");
    let account_id = account_id
        .filter(|s| !s.is_empty())
        .ok_or("Chưa cấu hình GitLab account cho project. Hãy link account trong cài đặt project.")?;

    let acc_row = sqlx::query("SELECT host FROM gitlab_accounts WHERE id = ?")
        .bind(&account_id)
        .fetch_one(db)
        .await
        .map_err(|_| "Không tìm thấy GitLab account đã link.".to_string())?;
    let host: Option<String> = acc_row.get("host");

    let pat = secrets::get_gitlab_account_pat(&account_id)
        .map_err(|_| "GitLab account chưa có PAT. Hãy nhập lại token trong cài đặt.".to_string())?;

    gitlab::GitlabClient::new(
        host.as_deref(),
        repo.gitlab_project_id,
        repo.gitlab_project_path.as_deref(),
        pat,
    )
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

/// Fetch an issue + its human comments from GitHub and render the task markdown.
/// Shared by `fetch_issue` (new run) and `refetch_run` (overwrite in place).
async fn build_issue_markdown(
    client: &octocrab::Octocrab,
    owner: &str,
    repo: &str,
    issue_number: u64,
) -> Result<String, String> {
    // Fetch issue
    let issue = client
        .issues(owner, repo)
        .get(issue_number)
        .await
        .map_err(|e| e.to_string())?;

    // Fetch comments
    let comments = client
        .issues(owner, repo)
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

    Ok(md)
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

    let repo = load_repo_identity(db.inner(), &repo_id).await?;
    let repo_slug = repo.slug();

    // Branch by provider (FR-005/BR-001). Both branches namespace the task dir
    // by repo_slug (BR-008/AC-10) so distinct repos never collide.
    let md = match repo.provider.as_str() {
        "gitlab" => {
            let client = gitlab_client_for(db.inner(), &project_id, &repo).await?;
            client.build_issue_markdown(issue_number).await?
        }
        _ => {
            let owner = repo
                .github_owner
                .clone()
                .ok_or("Repo has no GitHub owner configured")?;
            let gh_repo = repo
                .github_repo
                .clone()
                .ok_or("Repo has no GitHub repo configured")?;
            let client = github::client_for_project(db.inner(), &project_id)
                .await
                .map_err(|e| e.to_string())?;
            build_issue_markdown(&client, &owner, &gh_repo, issue_number).await?
        }
    };

    // Write file: .devdy/tasks/<repo_slug>/issue-<n>/issue.md
    let task_dir = Path::new(&project_path)
        .join(".devdy")
        .join("tasks")
        .join(&repo_slug)
        .join(format!("issue-{}", issue_number));
    fs::create_dir_all(&task_dir).map_err(|e| e.to_string())?;
    let file_path = task_dir.join("issue.md");
    fs::write(&file_path, &md).map_err(|e| e.to_string())?;

    // Use the global default engine (per-project engine has been removed).
    let engine = crate::commands::settings::resolve_default_engine(db.inner()).await;

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
        pinned: false,
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

/// Fetch a PR (metadata, diffs, comments, reviews, inline comments) from GitHub
/// and render the task markdown. Returns the markdown plus the resolved linked
/// issue number. Shared by `fetch_pr` (new run) and `refetch_run` (overwrite).
async fn build_pr_markdown(
    client: &octocrab::Octocrab,
    owner: &str,
    repo: &str,
    pr_number: u64,
    linked_issue: Option<u64>,
) -> Result<(String, u64), String> {
    // Fetch PR
    let pr = client
        .pulls(owner, repo)
        .get(pr_number)
        .await
        .map_err(|e| e.to_string())?;

    // Resolve linked issue: explicit param > GitHub "Development" linkage (GraphQL)
    let linked_issue_number = match linked_issue {
        Some(n) => n,
        None => match detect_linked_issue(client, owner, repo, pr_number).await {
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
        .pulls(owner, repo)
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
        .issues(owner, repo)
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
    if let Ok(first_page) = client.pulls(owner, repo).list_reviews(pr_number).per_page(100).send().await {
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
    if let Ok(first_page) = client.pulls(owner, repo).list_comments(Some(pr_number)).per_page(100).send().await {
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

    Ok((md, linked_issue_number))
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

    let repo = load_repo_identity(db.inner(), &repo_id).await?;
    let repo_slug = repo.slug();

    // Branch by provider (FR-005/BR-001).
    let (md, linked_issue_number) = match repo.provider.as_str() {
        "gitlab" => {
            let client = gitlab_client_for(db.inner(), &project_id, &repo).await?;
            client.build_mr_markdown(pr_number, linked_issue).await?
        }
        _ => {
            let owner = repo
                .github_owner
                .clone()
                .ok_or("Repo has no GitHub owner configured")?;
            let gh_repo = repo
                .github_repo
                .clone()
                .ok_or("Repo has no GitHub repo configured")?;
            let client = github::client_for_project(db.inner(), &project_id)
                .await
                .map_err(|e| e.to_string())?;
            build_pr_markdown(&client, &owner, &gh_repo, pr_number, linked_issue).await?
        }
    };

    // Write file: .devdy/tasks/<repo_slug>/issue-<linked>/<file>.md
    // GitHub keeps its historical `pr-<n>.md` name; GitLab uses `mr-<n>.md`.
    let file_name = match repo.provider.as_str() {
        "gitlab" => format!("mr-{}.md", pr_number),
        _ => format!("pr-{}.md", pr_number),
    };
    let task_dir = Path::new(&project_path)
        .join(".devdy")
        .join("tasks")
        .join(&repo_slug)
        .join(format!("issue-{}", linked_issue_number));
    fs::create_dir_all(&task_dir).map_err(|e| e.to_string())?;
    let file_path = task_dir.join(file_name);
    fs::write(&file_path, &md).map_err(|e| e.to_string())?;

    let engine = crate::commands::settings::resolve_default_engine(db.inner()).await;

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
        pinned: false,
    })
}

/// Re-fetch fresh PR/issue content from GitHub for an EXISTING run and overwrite
/// its input markdown file in place. The run record, its AI output, and session
/// are left untouched — so the user keeps the existing result and can continue
/// working with the refreshed context. Returns the (unchanged) run record.
#[tauri::command]
pub async fn refetch_run(db: State<'_, Db>, run_id: String) -> Result<RunRecord, String> {
    use sqlx::Row;

    let row = sqlx::query(
        "SELECT id, project_id, repo_id, type, ref_number, status, engine, input_path, output_path, session_id, started_at, finished_at, created_at, title, pinned FROM runs WHERE id = ?",
    )
    .bind(&run_id)
    .fetch_one(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    let project_id: String = row.get("project_id");
    let repo_id: Option<String> = row.get("repo_id");
    let run_type: String = row.get("type");
    let ref_number: Option<i64> = row.get("ref_number");
    let input_path: Option<String> = row.get("input_path");

    let repo_id_val = repo_id.clone().ok_or("Run has no repository configured")?;
    let ref_num = ref_number.ok_or("Run has no issue/PR number")? as u64;
    let input_path_val = input_path.clone().ok_or("Run has no input file to refresh")?;

    let repo = load_repo_identity(db.inner(), &repo_id_val).await?;

    // Provider derived from the run's repo (BR-007). Refetch overwrites the
    // existing input_path in place; run record/output/session stay untouched.
    let md = match repo.provider.as_str() {
        "gitlab" => {
            let client = gitlab_client_for(db.inner(), &project_id, &repo).await?;
            match run_type.as_str() {
                "analyze_issue" => client.build_issue_markdown(ref_num).await?,
                "review_pr" => {
                    // Reuse the linked issue from the existing frontmatter so the
                    // refresh never re-derives closes_issues or hits NO_LINKED_ISSUE.
                    let existing = fs::read_to_string(&input_path_val).unwrap_or_default();
                    let linked = parse_frontmatter_u64(&existing, "linked_issue");
                    client.build_mr_markdown(ref_num, linked).await?.0
                }
                other => return Err(format!("Cannot re-fetch a run of type '{}'", other)),
            }
        }
        _ => {
            let owner = repo
                .github_owner
                .clone()
                .ok_or("Repo has no GitHub owner configured")?;
            let gh_repo = repo
                .github_repo
                .clone()
                .ok_or("Repo has no GitHub repo configured")?;
            let client = github::client_for_project(db.inner(), &project_id)
                .await
                .map_err(|e| e.to_string())?;
            match run_type.as_str() {
                "analyze_issue" => build_issue_markdown(&client, &owner, &gh_repo, ref_num).await?,
                "review_pr" => {
                    let existing = fs::read_to_string(&input_path_val).unwrap_or_default();
                    let linked = parse_frontmatter_u64(&existing, "linked_issue");
                    build_pr_markdown(&client, &owner, &gh_repo, ref_num, linked).await?.0
                }
                other => return Err(format!("Cannot re-fetch a run of type '{}'", other)),
            }
        }
    };

    fs::write(&input_path_val, &md).map_err(|e| e.to_string())?;

    Ok(RunRecord {
        id: row.get("id"),
        project_id,
        repo_id,
        run_type,
        ref_number,
        status: row.get("status"),
        engine: row.get("engine"),
        input_path,
        output_path: row.get("output_path"),
        session_id: row.get("session_id"),
        started_at: row.get("started_at"),
        finished_at: row.get("finished_at"),
        created_at: row.get("created_at"),
        title: row.get("title"),
        pinned: row.get::<i64, _>("pinned") != 0,
    })
}

/// Parse a numeric `key: value` field from a leading YAML-ish frontmatter block.
fn parse_frontmatter_u64(content: &str, key: &str) -> Option<u64> {
    let prefix = format!("{}:", key);
    content
        .lines()
        .find_map(|line| line.trim().strip_prefix(&prefix))
        .and_then(|v| v.trim().parse::<u64>().ok())
}

#[tauri::command]
pub async fn list_runs(
    db: State<'_, Db>,
    project_id: String,
) -> Result<Vec<RunRecord>, String> {
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT id, project_id, repo_id, type, ref_number, status, engine, input_path, output_path, session_id, started_at, finished_at, created_at, title, pinned
         FROM runs WHERE project_id = ? ORDER BY pinned DESC, created_at DESC LIMIT 50"
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
        pinned: row.get::<i64, _>("pinned") != 0,
    }).collect())
}

/// A pull request awaiting the current user's review, aggregated across every
/// configured GitHub account, plus its mapping to a devdy project (if any).
#[derive(Debug, Serialize, Clone)]
pub struct PrInboxItem {
    /// Account whose review request surfaced this PR (label for display).
    pub account_id: String,
    pub account_label: String,
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub html_url: String,
    pub author_login: String,
    pub updated_at: String,
    /// True when a devdy repo row matches owner/repo — enables the Review action.
    pub mapped: bool,
    pub project_id: Option<String>,
    pub repo_id: Option<String>,
    pub project_name: Option<String>,
    /// Most recent existing `review_pr` run for this PR in the mapped project
    /// (if any) — lets the UI link to it and avoid spawning a duplicate session.
    pub existing_run_id: Option<String>,
    pub existing_run_status: Option<String>,
}

/// Parse `owner`/`repo` from a GitHub API `repository_url`
/// (`https://api.github.com/repos/{owner}/{repo}`).
fn parse_owner_repo(repository_url: &str) -> Option<(String, String)> {
    let tail = repository_url.split("/repos/").nth(1)?;
    let mut parts = tail.trim_end_matches('/').splitn(2, '/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    if owner.is_empty() || repo.is_empty() {
        None
    } else {
        Some((owner, repo))
    }
}

/// List every open PR where the signed-in user is a requested reviewer, gathered
/// across all validated GitHub accounts and mapped to devdy projects when the
/// repo is already tracked. Read-only; a failing account is skipped, not fatal.
#[tauri::command]
pub async fn list_review_requested_prs(db: State<'_, Db>) -> Result<Vec<PrInboxItem>, String> {
    use sqlx::Row;

    // Accounts that have been validated (username present). A PAT is required to
    // build a client, so skip any account missing one.
    let account_rows = sqlx::query(
        "SELECT id, label, username FROM github_accounts WHERE username IS NOT NULL AND username != '' ORDER BY created_at ASC",
    )
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    // Dedup a PR that multiple accounts are asked to review onto its first hit.
    let mut seen: std::collections::HashSet<(String, String, u64)> = std::collections::HashSet::new();
    let mut items: Vec<PrInboxItem> = Vec::new();

    for account in &account_rows {
        let account_id: String = account.get("id");
        let account_label: String = account.get("label");
        if !secrets::has_account_pat(&account_id) {
            continue;
        }
        let client = match github::client_for_account(&account_id) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // `@me` resolves to the token's own user, so each account contributes its
        // own review requests without us needing to know the login here.
        // Use `user-review-requested` (NOT `review-requested`): the latter also
        // matches PRs requested from a TEAM the user belongs to, surfacing PRs
        // the user was never personally asked to review. We only want direct
        // requests. See GitHub search docs: user-review-requested = "directly
        // been asked to review".
        let query = "is:open is:pr user-review-requested:@me archived:false";
        let first_page = match client
            .search()
            .issues_and_pull_requests(query)
            .per_page(100u8)
            .send()
            .await
        {
            Ok(p) => p,
            Err(_) => continue, // rate limit / bad PAT: skip this account, keep the rest
        };
        let issues = client.all_pages(first_page).await.unwrap_or_default();

        for issue in issues {
            let (owner, repo) = match parse_owner_repo(issue.repository_url.as_str()) {
                Some(pair) => pair,
                None => continue,
            };
            let number = issue.number;
            let key = (owner.clone(), repo.clone(), number);
            if !seen.insert(key) {
                continue;
            }

            // Map to a devdy project via a tracked repo row (case-insensitive).
            let mapping = sqlx::query(
                "SELECT r.id AS repo_id, r.project_id AS project_id, p.name AS project_name
                 FROM repos r JOIN projects p ON p.id = r.project_id
                 WHERE lower(r.github_owner) = lower(?) AND lower(r.github_repo) = lower(?)
                 LIMIT 1",
            )
            .bind(&owner)
            .bind(&repo)
            .fetch_optional(db.inner())
            .await
            .map_err(|e| e.to_string())?;

            let (mapped, project_id, repo_id, project_name) = match mapping {
                Some(row) => (
                    true,
                    Some(row.get::<String, _>("project_id")),
                    Some(row.get::<String, _>("repo_id")),
                    Some(row.get::<String, _>("project_name")),
                ),
                None => (false, None, None, None),
            };

            // Link the latest existing review run for this PR (same project +
            // ref_number) so the UI can resume it instead of duplicating.
            let (existing_run_id, existing_run_status) = match &project_id {
                Some(pid) => {
                    let run_row = sqlx::query(
                        "SELECT id, status FROM runs
                         WHERE project_id = ? AND type = 'review_pr' AND ref_number = ?
                         ORDER BY created_at DESC LIMIT 1",
                    )
                    .bind(pid)
                    .bind(number as i64)
                    .fetch_optional(db.inner())
                    .await
                    .map_err(|e| e.to_string())?;
                    match run_row {
                        Some(r) => (
                            Some(r.get::<String, _>("id")),
                            Some(r.get::<String, _>("status")),
                        ),
                        None => (None, None),
                    }
                }
                None => (None, None),
            };

            items.push(PrInboxItem {
                account_id: account_id.clone(),
                account_label: account_label.clone(),
                owner,
                repo,
                number,
                title: issue.title,
                html_url: issue.html_url.to_string(),
                author_login: issue.user.login,
                updated_at: issue.updated_at.to_rfc3339(),
                mapped,
                project_id,
                repo_id,
                project_name,
                existing_run_id,
                existing_run_status,
            });
        }
    }

    // Most recently updated first.
    items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(items)
}

// ---------------------------------------------------------------------------
// GitHub Projects V2 board tracking (milestone dashboard + Gantt).
// Read-only: resolve a board's fields for the settings mapper, then aggregate
// open issues across all repos of a project and enrich each with the board's
// Start / Deadline / Status field values.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BoardField {
    pub id: String,
    pub name: String,
    pub data_type: String,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BoardInfo {
    pub id: String,
    pub title: String,
    pub url: String,
    pub number: i64,
    pub owner: String,
    pub owner_type: String,
    pub fields: Vec<BoardField>,
}

#[derive(Debug, Serialize, Clone)]
pub struct LabelRow {
    pub name: String,
    pub color: String,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct IssueRow {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub html_url: String,
    pub created_at: String,
    pub updated_at: String,
    pub comments: u64,
    pub assignees: Vec<String>,
    pub labels: Vec<LabelRow>,
    pub milestone_title: Option<String>,
    pub milestone_due_on: Option<String>,
    pub start_date: Option<String>,
    pub deadline: Option<String>,
    pub status: Option<String>,
    /// GitHub Projects single-select option colour (enum: GRAY/BLUE/GREEN/...),
    /// used to tint the status badge like the GitHub board. None if unknown.
    pub status_color: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MilestoneGroup {
    pub title: String,
    pub due_on: Option<String>,
    pub repos: Vec<String>,
    pub open_count: u64,
    pub closed_count: u64,
    pub issues: Vec<IssueRow>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MilestoneBoard {
    pub milestones: Vec<MilestoneGroup>,
    pub fetched_at: String,
    pub board_linked: bool,
    pub truncated: bool,
    pub skipped_non_github: bool,
}

/// Parse a GitHub Projects V2 board URL into (owner_type, owner, number).
/// Accepts `https://github.com/orgs/<owner>/projects/<n>` and the `users/` form.
fn parse_board_url(url: &str) -> Result<(String, String, i64), String> {
    let trimmed = url.trim().trim_end_matches('/');
    let path = trimmed
        .split("github.com/")
        .nth(1)
        .ok_or_else(|| "Invalid board URL".to_string())?;
    let parts: Vec<&str> = path.split('/').collect();
    // Expect: [orgs|users], <owner>, projects, <number>
    if parts.len() < 4 || parts[2] != "projects" {
        return Err("Invalid board URL. Expected: https://github.com/orgs/<owner>/projects/<number>".to_string());
    }
    let owner_type = match parts[0] {
        "orgs" => "org",
        "users" => "user",
        _ => return Err("Board URL must be of the form /orgs/... or /users/...".to_string()),
    };
    let owner = parts[1].to_string();
    let number: i64 = parts[3]
        .parse()
        .map_err(|_| "Invalid project number in URL".to_string())?;
    Ok((owner_type.to_string(), owner, number))
}

/// Warn early if the linked account clearly lacks Projects access. Classic PATs
/// expose scopes via `x-oauth-scopes`; fine-grained tokens report none, so we
/// only block when scopes are present AND lack a project/repo scope.
async fn check_project_scope(db: &Db, project_id: &str) -> Result<(), String> {
    use sqlx::Row;
    let row = sqlx::query("SELECT github_account_id FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_one(db)
        .await
        .map_err(|e| e.to_string())?;
    let account_id: Option<String> = row.get("github_account_id");
    let Some(account_id) = account_id else {
        return Err("This project has no linked GitHub account. Link one in Settings.".to_string());
    };
    let acc = sqlx::query("SELECT scopes FROM github_accounts WHERE id = ?")
        .bind(&account_id)
        .fetch_one(db)
        .await
        .map_err(|e| e.to_string())?;
    let scopes: Option<String> = acc.get("scopes");
    if let Some(scopes) = scopes {
        let scopes = scopes.to_lowercase();
        if !scopes.trim().is_empty()
            && !scopes.contains("read:project")
            && !scopes.contains("project")
        {
            return Err(
                "The GitHub account is missing the 'read:project' scope needed to read GitHub Projects V2. Update the PAT to include read:project.".to_string(),
            );
        }
    }
    Ok(())
}

/// Resolve a board by URL and return its fields for the settings field-mapper.
#[tauri::command]
pub async fn resolve_project_board(
    db: State<'_, Db>,
    project_id: String,
    url: String,
) -> Result<BoardInfo, String> {
    let (owner_type, owner, number) = parse_board_url(&url)?;
    check_project_scope(db.inner(), &project_id).await?;
    let client = github::client_for_project(db.inner(), &project_id)
        .await
        .map_err(|e| e.to_string())?;

    let root = if owner_type == "org" { "organization" } else { "user" };
    let query = format!(
        "query($login:String!,$number:Int!){{ {root}(login:$login){{ projectV2(number:$number){{ id title url fields(first:50){{ nodes{{ ... on ProjectV2FieldCommon{{ id name dataType }} }} }} }} }} }}"
    );
    let payload = serde_json::json!({ "query": query, "variables": { "login": owner, "number": number } });
    let resp: serde_json::Value = client.graphql(&payload).await.map_err(|e| e.to_string())?;
    if let Some(errors) = resp.get("errors").and_then(|e| e.as_array()) {
        if !errors.is_empty() {
            let msg = errors[0].get("message").and_then(|m| m.as_str()).unwrap_or("GraphQL error");
            return Err(format!("Failed to read board: {msg}"));
        }
    }
    let board = resp
        .pointer(&format!("/data/{root}/projectV2"))
        .filter(|v| !v.is_null())
        .ok_or_else(|| "Project V2 board not found (check the URL and access permissions).".to_string())?;

    let fields = board
        .pointer("/fields/nodes")
        .and_then(|n| n.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|f| {
                    let id = f.get("id")?.as_str()?.to_string();
                    let name = f.get("name")?.as_str()?.to_string();
                    let data_type = f
                        .get("dataType")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(BoardField { id, name, data_type })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(BoardInfo {
        id: board.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        title: board.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        url: board.get("url").and_then(|v| v.as_str()).unwrap_or(&url).to_string(),
        number,
        owner,
        owner_type,
        fields,
    })
}

#[derive(Default)]
struct BoardMappings {
    board_id: Option<String>,
    start_field_id: Option<String>,
    deadline_field_id: Option<String>,
    status_field_id: Option<String>,
}

fn parse_board_mappings(json: &str) -> BoardMappings {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    let get = |k: &str| v.get(k).and_then(|x| x.as_str()).map(|s| s.to_string());
    BoardMappings {
        board_id: get("boardId"),
        start_field_id: get("startFieldId"),
        deadline_field_id: get("deadlineFieldId"),
        status_field_id: get("statusFieldId"),
    }
}

/// Board enrichment value keyed by `owner/repo#number` (lowercased).
#[derive(Default, Clone)]
struct BoardItemFields {
    start: Option<String>,
    deadline: Option<String>,
    status: Option<String>,
    status_color: Option<String>,
}

/// Add `days` to a `YYYY-MM-DD` date, returning the ISO date string.
fn add_days(date: &str, days: i64) -> Option<String> {
    let d = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    Some((d + chrono::Duration::days(days)).format("%Y-%m-%d").to_string())
}

/// Fetch the status single-select field's option colours (option name -> GitHub
/// colour enum, e.g. "GREEN"), so the UI can tint the badge like the board.
async fn fetch_status_colors(
    client: &octocrab::Octocrab,
    board_id: &str,
    status_field_id: &str,
) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let query = "query($id:ID!){ node(id:$id){ ... on ProjectV2{ fields(first:50){ nodes{ __typename ... on ProjectV2SingleSelectField{ id options{ name color } } } } } } }";
    let payload = serde_json::json!({ "query": query, "variables": { "id": board_id } });
    let resp: serde_json::Value = match client.graphql(&payload).await {
        Ok(v) => v,
        Err(_) => return map,
    };
    if let Some(nodes) = resp.pointer("/data/node/fields/nodes").and_then(|n| n.as_array()) {
        for f in nodes {
            if f.get("id").and_then(|v| v.as_str()) != Some(status_field_id) {
                continue;
            }
            if let Some(options) = f.get("options").and_then(|o| o.as_array()) {
                for opt in options {
                    if let (Some(name), Some(color)) = (
                        opt.get("name").and_then(|v| v.as_str()),
                        opt.get("color").and_then(|v| v.as_str()),
                    ) {
                        map.insert(name.to_lowercase(), color.to_string());
                    }
                }
            }
        }
    }
    map
}

/// Fetch all board items and build a map of field values keyed by issue.
async fn fetch_board_item_map(
    client: &octocrab::Octocrab,
    m: &BoardMappings,
    status_colors: &std::collections::HashMap<String, String>,
) -> std::collections::HashMap<String, BoardItemFields> {
    let mut map = std::collections::HashMap::new();
    let Some(board_id) = &m.board_id else { return map };
    let mut cursor: Option<String> = None;
    for _ in 0..20 {
        let query = "query($id:ID!,$cursor:String){ node(id:$id){ ... on ProjectV2{ items(first:100, after:$cursor){ pageInfo{ hasNextPage endCursor } nodes{ content{ __typename ... on Issue{ number repository{ name owner{ login } } } } fieldValues(first:30){ nodes{ __typename ... on ProjectV2ItemFieldDateValue{ date field{ ... on ProjectV2FieldCommon{ id } } } ... on ProjectV2ItemFieldSingleSelectValue{ name field{ ... on ProjectV2FieldCommon{ id } } } ... on ProjectV2ItemFieldIterationValue{ startDate duration field{ ... on ProjectV2FieldCommon{ id } } } } } } } } } }";
        let payload = serde_json::json!({ "query": query, "variables": { "id": board_id, "cursor": cursor } });
        let resp: serde_json::Value = match client.graphql(&payload).await {
            Ok(v) => v,
            Err(_) => break,
        };
        let items = resp.pointer("/data/node/items");
        let Some(items) = items else { break };
        if let Some(nodes) = items.pointer("/nodes").and_then(|n| n.as_array()) {
            for item in nodes {
                let content = item.get("content");
                let is_issue = content
                    .and_then(|c| c.get("__typename"))
                    .and_then(|t| t.as_str())
                    == Some("Issue");
                if !is_issue {
                    continue;
                }
                let number = content.and_then(|c| c.get("number")).and_then(|n| n.as_u64());
                let repo = content
                    .and_then(|c| c.pointer("/repository/name"))
                    .and_then(|s| s.as_str());
                let owner = content
                    .and_then(|c| c.pointer("/repository/owner/login"))
                    .and_then(|s| s.as_str());
                let (Some(number), Some(repo), Some(owner)) = (number, repo, owner) else { continue };
                let key = format!("{}/{}#{}", owner.to_lowercase(), repo.to_lowercase(), number);
                let mut fields = BoardItemFields::default();
                if let Some(fv_nodes) = item.pointer("/fieldValues/nodes").and_then(|n| n.as_array()) {
                    for fv in fv_nodes {
                        let field_id = fv.pointer("/field/id").and_then(|v| v.as_str());
                        let Some(field_id) = field_id else { continue };
                        let typename = fv.get("__typename").and_then(|t| t.as_str()).unwrap_or("");
                        match typename {
                            "ProjectV2ItemFieldDateValue" => {
                                let date = fv.get("date").and_then(|d| d.as_str()).map(|s| {
                                    // Date scalar is YYYY-MM-DD; keep only the date part.
                                    s.split('T').next().unwrap_or(s).to_string()
                                });
                                if Some(field_id) == m.start_field_id.as_deref() {
                                    fields.start = date.clone();
                                }
                                if Some(field_id) == m.deadline_field_id.as_deref() {
                                    fields.deadline = date;
                                }
                            }
                            "ProjectV2ItemFieldSingleSelectValue" => {
                                if Some(field_id) == m.status_field_id.as_deref() {
                                    let name = fv.get("name").and_then(|n| n.as_str()).map(|s| s.to_string());
                                    fields.status_color = name
                                        .as_ref()
                                        .and_then(|n| status_colors.get(&n.to_lowercase()).cloned());
                                    fields.status = name;
                                }
                            }
                            "ProjectV2ItemFieldIterationValue" => {
                                let start = fv.get("startDate").and_then(|d| d.as_str());
                                let duration = fv.get("duration").and_then(|d| d.as_i64());
                                if let Some(start) = start {
                                    if Some(field_id) == m.start_field_id.as_deref() {
                                        fields.start = Some(start.to_string());
                                    }
                                    if Some(field_id) == m.deadline_field_id.as_deref() {
                                        fields.deadline = duration
                                            .and_then(|d| add_days(start, d))
                                            .or_else(|| Some(start.to_string()));
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                map.insert(key, fields);
            }
        }
        let has_next = items
            .pointer("/pageInfo/hasNextPage")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !has_next {
            break;
        }
        cursor = items
            .pointer("/pageInfo/endCursor")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if cursor.is_none() {
            break;
        }
    }
    map
}

/// Aggregate open issues across all GitHub repos of a project, grouped by
/// milestone and enriched with the linked board's Start/Deadline/Status fields.
#[tauri::command]
pub async fn list_milestone_board(
    db: State<'_, Db>,
    project_id: String,
) -> Result<MilestoneBoard, String> {
    use sqlx::Row;

    // 1) Load repos for the project.
    let repo_rows = sqlx::query(
        "SELECT name, github_owner, github_repo, provider FROM repos WHERE project_id = ?",
    )
    .bind(&project_id)
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    // 2) Load board config (if any).
    let proj = sqlx::query(
        "SELECT github_project_field_mappings FROM projects WHERE id = ?",
    )
    .bind(&project_id)
    .fetch_one(db.inner())
    .await
    .map_err(|e| e.to_string())?;
    let mappings_json: Option<String> = proj.get("github_project_field_mappings");
    let mappings = mappings_json.as_deref().map(parse_board_mappings);
    let board_linked = mappings
        .as_ref()
        .map(|m| m.board_id.is_some())
        .unwrap_or(false);

    let client = github::client_for_project(db.inner(), &project_id)
        .await
        .map_err(|e| e.to_string())?;

    // 3) Board enrichment map (owner/repo#number -> fields), with status colours.
    let board_map = if let Some(m) = &mappings {
        let status_colors = match (&m.board_id, &m.status_field_id) {
            (Some(bid), Some(sfid)) => fetch_status_colors(&client, bid, sfid).await,
            _ => std::collections::HashMap::new(),
        };
        fetch_board_item_map(&client, m, &status_colors).await
    } else {
        std::collections::HashMap::new()
    };

    let mut truncated = false;
    let mut skipped_non_github = false;

    // milestone title (lowercased) -> aggregated summary
    struct MsAcc {
        title: String,
        open: u64,
        closed: u64,
        due: Option<String>,
    }
    let mut ms_summary: std::collections::HashMap<String, MsAcc> = std::collections::HashMap::new();
    let mut all_issues: Vec<IssueRow> = Vec::new();

    for repo in &repo_rows {
        let provider: Option<String> = repo.get("provider");
        let provider = provider.filter(|p| !p.is_empty()).unwrap_or_else(|| "github".to_string());
        if provider != "github" {
            skipped_non_github = true;
            continue;
        }
        let owner: Option<String> = repo.get("github_owner");
        let name: Option<String> = repo.get("github_repo");
        let (Some(owner), Some(name)) = (owner, name) else { continue };
        if owner.is_empty() || name.is_empty() {
            continue;
        }

        let mut cursor: Option<String> = None;
        let mut first_page = true;
        for page in 0..10 {
            let query = "query($owner:String!,$repo:String!,$cursor:String,$withMs:Boolean!){ repository(owner:$owner,name:$repo){ milestones(first:50, states:[OPEN,CLOSED]) @include(if:$withMs){ nodes{ title dueOn open:issues(states:OPEN){ totalCount } closed:issues(states:CLOSED){ totalCount } } } issues(first:100, after:$cursor, states:OPEN, orderBy:{field:UPDATED_AT,direction:DESC}){ pageInfo{ hasNextPage endCursor } nodes{ number title url createdAt updatedAt comments{ totalCount } milestone{ title dueOn } assignees(first:5){ nodes{ login } } labels(first:10){ nodes{ name color } } } } } }";
            let payload = serde_json::json!({
                "query": query,
                "variables": { "owner": owner, "repo": name, "cursor": cursor, "withMs": first_page }
            });
            let resp: serde_json::Value = match client.graphql(&payload).await {
                Ok(v) => v,
                Err(e) => return Err(format!("Failed to load issues for {owner}/{name}: {e}")),
            };
            if let Some(errors) = resp.get("errors").and_then(|e| e.as_array()) {
                if !errors.is_empty() {
                    let msg = errors[0].get("message").and_then(|m| m.as_str()).unwrap_or("GraphQL error");
                    return Err(format!("GraphQL error for {owner}/{name}: {msg}"));
                }
            }
            let repository = match resp.pointer("/data/repository") {
                Some(r) if !r.is_null() => r,
                _ => break,
            };

            if first_page {
                if let Some(ms_nodes) = repository.pointer("/milestones/nodes").and_then(|n| n.as_array()) {
                    for ms in ms_nodes {
                        let title = ms.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string();
                        if title.is_empty() {
                            continue;
                        }
                        let open = ms.pointer("/open/totalCount").and_then(|v| v.as_u64()).unwrap_or(0);
                        let closed = ms.pointer("/closed/totalCount").and_then(|v| v.as_u64()).unwrap_or(0);
                        let due = ms.get("dueOn").and_then(|d| d.as_str()).map(|s| s.to_string());
                        let key = title.to_lowercase();
                        let entry = ms_summary.entry(key).or_insert(MsAcc {
                            title: title.clone(),
                            open: 0,
                            closed: 0,
                            due: None,
                        });
                        entry.open += open;
                        entry.closed += closed;
                        // Keep the earliest due date when the same milestone spans repos.
                        entry.due = match (entry.due.take(), due) {
                            (Some(a), Some(b)) => Some(if a <= b { a } else { b }),
                            (Some(a), None) => Some(a),
                            (None, b) => b,
                        };
                    }
                }
            }

            let issue_nodes = repository.pointer("/issues/nodes").and_then(|n| n.as_array());
            if let Some(nodes) = issue_nodes {
                for issue in nodes {
                    let number = issue.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
                    let key = format!("{}/{}#{}", owner.to_lowercase(), name.to_lowercase(), number);
                    let bf = board_map.get(&key).cloned().unwrap_or_default();
                    let milestone_due = issue
                        .pointer("/milestone/dueOn")
                        .and_then(|d| d.as_str())
                        .map(|s| s.to_string());
                    let assignees = issue
                        .pointer("/assignees/nodes")
                        .and_then(|n| n.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.get("login").and_then(|l| l.as_str()).map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();
                    let labels = issue
                        .pointer("/labels/nodes")
                        .and_then(|n| n.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| {
                                    Some(LabelRow {
                                        name: x.get("name")?.as_str()?.to_string(),
                                        color: x.get("color").and_then(|c| c.as_str()).unwrap_or("").to_string(),
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    all_issues.push(IssueRow {
                        repo: name.clone(),
                        number,
                        title: issue.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string(),
                        html_url: issue.get("url").and_then(|u| u.as_str()).unwrap_or("").to_string(),
                        created_at: issue.get("createdAt").and_then(|t| t.as_str()).unwrap_or("").to_string(),
                        updated_at: issue.get("updatedAt").and_then(|t| t.as_str()).unwrap_or("").to_string(),
                        comments: issue.pointer("/comments/totalCount").and_then(|v| v.as_u64()).unwrap_or(0),
                        assignees,
                        labels,
                        milestone_title: issue
                            .pointer("/milestone/title")
                            .and_then(|t| t.as_str())
                            .map(|s| s.to_string()),
                        milestone_due_on: milestone_due,
                        start_date: bf.start,
                        deadline: bf.deadline,
                        status: bf.status,
                        status_color: bf.status_color,
                    });
                }
            }

            let has_next = repository
                .pointer("/issues/pageInfo/hasNextPage")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !has_next {
                break;
            }
            if page == 9 {
                // Hit the 1000-issue cap while more remained.
                truncated = true;
                break;
            }
            cursor = repository
                .pointer("/issues/pageInfo/endCursor")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            first_page = false;
            if cursor.is_none() {
                break;
            }
        }
    }

    // 4) Group issues by milestone title.
    const NO_MS: &str = "__no_milestone__";
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, MilestoneGroup> = std::collections::HashMap::new();
    for issue in all_issues {
        let key = issue
            .milestone_title
            .as_ref()
            .map(|t| t.to_lowercase())
            .unwrap_or_else(|| NO_MS.to_string());
        if !groups.contains_key(&key) {
            order.push(key.clone());
            let (title, due, open, closed) = if key == NO_MS {
                ("No milestone".to_string(), None, 0, 0)
            } else if let Some(acc) = ms_summary.get(&key) {
                (acc.title.clone(), acc.due.clone(), acc.open, acc.closed)
            } else {
                (issue.milestone_title.clone().unwrap_or_default(), issue.milestone_due_on.clone(), 0, 0)
            };
            groups.insert(
                key.clone(),
                MilestoneGroup {
                    title,
                    due_on: due,
                    repos: Vec::new(),
                    open_count: open,
                    closed_count: closed,
                    issues: Vec::new(),
                },
            );
        }
        let g = groups.get_mut(&key).unwrap();
        if !g.repos.contains(&issue.repo) {
            g.repos.push(issue.repo.clone());
        }
        g.issues.push(issue);
    }
    // For the no-milestone bucket, open_count = number of listed open issues.
    if let Some(g) = groups.get_mut(NO_MS) {
        g.open_count = g.issues.len() as u64;
    }

    let milestones: Vec<MilestoneGroup> =
        order.into_iter().filter_map(|k| groups.remove(&k)).collect();

    Ok(MilestoneBoard {
        milestones,
        fetched_at: chrono::Utc::now().to_rfc3339(),
        board_linked,
        truncated,
        skipped_non_github,
    })
}
