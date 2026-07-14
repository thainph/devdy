use crate::db::Db;
use crate::secrets;
use serde::Serialize;
use sqlx::Row;
use tauri::State;
use uuid::Uuid;

const DEFAULT_HOST: &str = "https://gitlab.com";

#[derive(Debug, Serialize, Clone)]
pub struct GitlabAccount {
    pub id: String,
    pub label: String,
    pub username: Option<String>,
    pub host: Option<String>,
    pub email: Option<String>,
    pub scopes: Vec<String>,
    pub has_pat: bool,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct PatValidation {
    pub username: String,
    pub email: Option<String>,
    pub scopes: Vec<String>,
}

fn row_to_account(row: &sqlx::sqlite::SqliteRow) -> GitlabAccount {
    let id: String = row.get("id");
    let scopes_str: Option<String> = row.get("scopes");
    let scopes = scopes_str
        .map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let has_pat = secrets::has_gitlab_account_pat(&id);
    GitlabAccount {
        label: row.get("label"),
        username: row.get("username"),
        host: row.get("host"),
        email: row.get("email"),
        scopes,
        has_pat,
        created_at: row.get("created_at"),
        id,
    }
}

/// Normalize a caller-supplied host, falling back to gitlab.com when empty.
/// Trailing slashes are stripped so we can build `{host}/api/v4/...` cleanly.
fn normalize_host(host: Option<&str>) -> String {
    let h = host.map(|s| s.trim()).unwrap_or("");
    let h = if h.is_empty() { DEFAULT_HOST } else { h };
    h.trim_end_matches('/').to_string()
}

/// Validate a raw PAT against the GitLab API, returning the username + email.
///
/// GitLab uses the `PRIVATE-TOKEN` header (not `Bearer`) for personal access
/// tokens. Unlike GitHub, `GET /api/v4/user` does not expose the token scopes,
/// so `scopes` is intentionally left empty in phase 1.
async fn validate_token(host: &str, pat: &str) -> Result<PatValidation, String> {
    let client = reqwest::Client::new();
    let pat = pat.trim();
    let resp = client
        .get(format!("{}/api/v4/user", host))
        .header("PRIVATE-TOKEN", pat)
        .header("User-Agent", "devdy/0.1")
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = resp.status();
    if !status.is_success() {
        // GitLab returns 401 for both an invalid/expired token AND a token that
        // lacks the scope needed to read `/api/v4/user` (a repo-only token). Give
        // an actionable hint instead of the bare status.
        if status.as_u16() == 401 {
            return Err(format!(
                "GitLab API error: 401 Unauthorized. Kiểm tra: (1) token còn hạn và đúng; \
                 (2) token có scope `read_api` (hoặc `api`) — token chỉ có scope repository \
                 sẽ bị `/api/v4/user` từ chối; (3) host đúng ({host})."
            ));
        }
        return Err(format!("GitLab API error: {status} (host {host})"));
    }

    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let username = json["username"].as_str().unwrap_or("unknown").to_string();
    let email = json["email"].as_str().map(|s| s.to_string());

    Ok(PatValidation { username, email, scopes: Vec::new() })
}

#[tauri::command]
pub async fn list_gitlab_accounts(db: State<'_, Db>) -> Result<Vec<GitlabAccount>, String> {
    let rows = sqlx::query(
        "SELECT id, label, username, host, email, scopes, created_at FROM gitlab_accounts ORDER BY label"
    )
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows.iter().map(row_to_account).collect())
}

#[tauri::command]
pub async fn create_gitlab_account(
    db: State<'_, Db>,
    label: String,
    pat: String,
    host: Option<String>,
    email: Option<String>,
) -> Result<GitlabAccount, String> {
    let host = normalize_host(host.as_deref());
    // Validate first so we can store the username/email alongside the account.
    let validation = validate_token(&host, &pat).await?;

    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let scopes_str = validation.scopes.join(", ");
    // Prefer the caller-supplied email, fall back to the API-reported one.
    let email = email
        .filter(|e| !e.trim().is_empty())
        .or_else(|| validation.email.clone());

    sqlx::query(
        "INSERT INTO gitlab_accounts (id, label, username, host, email, scopes, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(&id)
    .bind(&label)
    .bind(&validation.username)
    .bind(&host)
    .bind(&email)
    .bind(&scopes_str)
    .bind(&now)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    secrets::set_gitlab_account_pat(&id, &pat).map_err(|e| e.to_string())?;

    Ok(GitlabAccount {
        id,
        label,
        username: Some(validation.username),
        host: Some(host),
        email,
        scopes: validation.scopes,
        has_pat: true,
        created_at: now,
    })
}

#[tauri::command]
pub async fn update_gitlab_account(
    db: State<'_, Db>,
    id: String,
    label: String,
    pat: Option<String>,
    host: Option<String>,
    email: Option<String>,
) -> Result<GitlabAccount, String> {
    let host = normalize_host(host.as_deref());
    // If a new PAT is supplied, re-validate and refresh username/email/scopes.
    if let Some(pat) = pat.as_ref().filter(|p| !p.trim().is_empty()) {
        let validation = validate_token(&host, pat).await?;
        let scopes_str = validation.scopes.join(", ");
        let email = email
            .filter(|e| !e.trim().is_empty())
            .or_else(|| validation.email.clone());
        sqlx::query(
            "UPDATE gitlab_accounts SET label = ?, username = ?, host = ?, email = ?, scopes = ? WHERE id = ?"
        )
        .bind(&label)
        .bind(&validation.username)
        .bind(&host)
        .bind(&email)
        .bind(&scopes_str)
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
        secrets::set_gitlab_account_pat(&id, pat).map_err(|e| e.to_string())?;
    } else {
        let email = email.filter(|e| !e.trim().is_empty());
        sqlx::query("UPDATE gitlab_accounts SET label = ?, host = ?, email = ? WHERE id = ?")
            .bind(&label)
            .bind(&host)
            .bind(&email)
            .bind(&id)
            .execute(db.inner())
            .await
            .map_err(|e| e.to_string())?;
    }

    let row = sqlx::query(
        "SELECT id, label, username, host, email, scopes, created_at FROM gitlab_accounts WHERE id = ?"
    )
    .bind(&id)
    .fetch_one(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(row_to_account(&row))
}

#[tauri::command]
pub async fn delete_gitlab_account(db: State<'_, Db>, id: String) -> Result<(), String> {
    let _ = secrets::delete_gitlab_account_pat(&id);
    // projects.gitlab_account_id is ON DELETE SET NULL, so linked projects simply unlink.
    sqlx::query("DELETE FROM gitlab_accounts WHERE id = ?")
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn validate_gitlab_account(
    db: State<'_, Db>,
    id: String,
) -> Result<PatValidation, String> {
    let pat = secrets::get_gitlab_account_pat(&id).map_err(|e| format!("No PAT stored: {}", e))?;

    let row = sqlx::query("SELECT host FROM gitlab_accounts WHERE id = ?")
        .bind(&id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    let host: Option<String> = row.get("host");
    let host = normalize_host(host.as_deref());

    let validation = validate_token(&host, &pat).await?;

    // Refresh cached username/email/scopes from the live check.
    let scopes_str = validation.scopes.join(", ");
    let _ = sqlx::query("UPDATE gitlab_accounts SET username = ?, email = ?, scopes = ? WHERE id = ?")
        .bind(&validation.username)
        .bind(&validation.email)
        .bind(&scopes_str)
        .bind(&id)
        .execute(db.inner())
        .await;

    Ok(validation)
}

#[tauri::command]
pub async fn set_project_gitlab_account(
    db: State<'_, Db>,
    project_id: String,
    account_id: Option<String>,
) -> Result<(), String> {
    sqlx::query("UPDATE projects SET gitlab_account_id = ? WHERE id = ?")
        .bind(&account_id)
        .bind(&project_id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}
