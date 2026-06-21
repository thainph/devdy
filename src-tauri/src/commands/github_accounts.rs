use crate::db::Db;
use crate::secrets;
use serde::Serialize;
use sqlx::Row;
use tauri::State;
use uuid::Uuid;

#[derive(Debug, Serialize, Clone)]
pub struct GithubAccount {
    pub id: String,
    pub label: String,
    pub username: Option<String>,
    pub scopes: Vec<String>,
    pub has_pat: bool,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct PatValidation {
    pub username: String,
    pub scopes: Vec<String>,
    pub has_repo_scope: bool,
}

fn row_to_account(row: &sqlx::sqlite::SqliteRow) -> GithubAccount {
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
    let has_pat = secrets::has_account_pat(&id);
    GithubAccount {
        label: row.get("label"),
        username: row.get("username"),
        scopes,
        has_pat,
        created_at: row.get("created_at"),
        id,
    }
}

/// Validate a raw PAT against the GitHub API, returning the login + token scopes.
async fn validate_token(pat: &str) -> Result<PatValidation, String> {
    let client = reqwest::Client::new();
    let resp = client
        .get("https://api.github.com/user")
        .header("Authorization", format!("Bearer {}", pat))
        .header("User-Agent", "devdy/0.1")
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Err(format!("GitHub API error: {}", resp.status()));
    }

    let scopes_header = resp
        .headers()
        .get("x-oauth-scopes")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let scopes: Vec<String> = scopes_header
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let username = json["login"].as_str().unwrap_or("unknown").to_string();
    let has_repo_scope = scopes.iter().any(|s| s == "repo" || s == "public_repo");

    Ok(PatValidation { username, scopes, has_repo_scope })
}

#[tauri::command]
pub async fn list_github_accounts(db: State<'_, Db>) -> Result<Vec<GithubAccount>, String> {
    let rows = sqlx::query(
        "SELECT id, label, username, scopes, created_at FROM github_accounts ORDER BY label"
    )
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows.iter().map(row_to_account).collect())
}

#[tauri::command]
pub async fn create_github_account(
    db: State<'_, Db>,
    label: String,
    pat: String,
) -> Result<GithubAccount, String> {
    // Validate first so we can store the login + scopes alongside the account.
    let validation = validate_token(&pat).await?;

    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let scopes_str = validation.scopes.join(", ");

    sqlx::query(
        "INSERT INTO github_accounts (id, label, username, scopes, created_at) VALUES (?, ?, ?, ?, ?)"
    )
    .bind(&id)
    .bind(&label)
    .bind(&validation.username)
    .bind(&scopes_str)
    .bind(&now)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    secrets::set_account_pat(&id, &pat).map_err(|e| e.to_string())?;

    Ok(GithubAccount {
        id,
        label,
        username: Some(validation.username),
        scopes: validation.scopes,
        has_pat: true,
        created_at: now,
    })
}

#[tauri::command]
pub async fn update_github_account(
    db: State<'_, Db>,
    id: String,
    label: String,
    pat: Option<String>,
) -> Result<GithubAccount, String> {
    // If a new PAT is supplied, re-validate and refresh username/scopes.
    if let Some(pat) = pat.as_ref().filter(|p| !p.trim().is_empty()) {
        let validation = validate_token(pat).await?;
        let scopes_str = validation.scopes.join(", ");
        sqlx::query("UPDATE github_accounts SET label = ?, username = ?, scopes = ? WHERE id = ?")
            .bind(&label)
            .bind(&validation.username)
            .bind(&scopes_str)
            .bind(&id)
            .execute(db.inner())
            .await
            .map_err(|e| e.to_string())?;
        secrets::set_account_pat(&id, pat).map_err(|e| e.to_string())?;
    } else {
        sqlx::query("UPDATE github_accounts SET label = ? WHERE id = ?")
            .bind(&label)
            .bind(&id)
            .execute(db.inner())
            .await
            .map_err(|e| e.to_string())?;
    }

    let row = sqlx::query(
        "SELECT id, label, username, scopes, created_at FROM github_accounts WHERE id = ?"
    )
    .bind(&id)
    .fetch_one(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(row_to_account(&row))
}

#[tauri::command]
pub async fn delete_github_account(db: State<'_, Db>, id: String) -> Result<(), String> {
    let _ = secrets::delete_account_pat(&id);
    // projects.github_account_id is ON DELETE SET NULL, so linked projects simply unlink.
    sqlx::query("DELETE FROM github_accounts WHERE id = ?")
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn validate_github_account(
    db: State<'_, Db>,
    id: String,
) -> Result<PatValidation, String> {
    let pat = secrets::get_account_pat(&id).map_err(|e| format!("No PAT stored: {}", e))?;
    let validation = validate_token(&pat).await?;

    // Refresh cached username/scopes from the live check.
    let scopes_str = validation.scopes.join(", ");
    let _ = sqlx::query("UPDATE github_accounts SET username = ?, scopes = ? WHERE id = ?")
        .bind(&validation.username)
        .bind(&scopes_str)
        .bind(&id)
        .execute(db.inner())
        .await;

    Ok(validation)
}

#[tauri::command]
pub async fn set_project_github_account(
    db: State<'_, Db>,
    project_id: String,
    account_id: Option<String>,
) -> Result<(), String> {
    sqlx::query("UPDATE projects SET github_account_id = ? WHERE id = ?")
        .bind(&account_id)
        .bind(&project_id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}
