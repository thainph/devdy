use crate::db::Db;
use crate::runs::sidecar::{apply_claude_config_dir, augment_command_path};
use serde::Serialize;
use serde_json::Value;
use sqlx::Row;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::process::Stdio;
use std::time::Duration;
use tauri::{AppHandle, Manager, State};
use tokio::process::Command;
use uuid::Uuid;

const STATUS_UNKNOWN: &str = "unknown";
const STATUS_READY: &str = "ready";
const STATUS_NEEDS_LOGIN: &str = "needs_login";
const STATUS_ERROR: &str = "error";

#[derive(Debug, Serialize, Clone)]
pub struct ClaudeAccount {
    pub id: String,
    pub label: String,
    pub config_dir: String,
    pub email: Option<String>,
    pub auth_method: Option<String>,
    pub api_provider: Option<String>,
    pub status: String,
    pub is_default: bool,
    pub last_checked_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct ClaudeValidation {
    pub logged_in: bool,
    pub status: String,
    pub email: Option<String>,
    pub auth_method: Option<String>,
    pub api_provider: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ClaudeRuntimeAccount {
    pub id: String,
    /// Human label, kept for diagnostics/logging even though the runtime only
    /// needs `id` + `config_dir`.
    #[allow(dead_code)]
    pub label: String,
    pub config_dir: String,
}

fn row_to_account(row: &sqlx::sqlite::SqliteRow) -> ClaudeAccount {
    ClaudeAccount {
        id: row.get("id"),
        label: row.get("label"),
        config_dir: row.get("config_dir"),
        email: row.get("email"),
        auth_method: row.get("auth_method"),
        api_provider: row.get("api_provider"),
        status: row.get("status"),
        is_default: row.get::<i64, _>("is_default") != 0,
        last_checked_at: row.get("last_checked_at"),
        created_at: row.get("created_at"),
    }
}

fn clean_label(label: &str) -> Result<String, String> {
    let label = label.trim();
    if label.is_empty() {
        Err("Claude account label is required".to_string())
    } else {
        Ok(label.to_string())
    }
}

fn claude_accounts_root(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|p| p.join("claude-accounts"))
        .map_err(|e| e.to_string())
}

fn account_config_dir(app: &AppHandle, id: &str) -> Result<PathBuf, String> {
    Ok(claude_accounts_root(app)?.join(id))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn applescript_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn claude_path_from_rows(rows: &[sqlx::sqlite::SqliteRow]) -> String {
    rows.iter()
        .find_map(|row| {
            let key: String = row.get("key");
            if key == "claude_path" {
                let value: String = row.get("value");
                Some(value)
            } else {
                None
            }
        })
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "claude".to_string())
}

async fn load_claude_path(db: &Db) -> Result<String, String> {
    let rows = sqlx::query("SELECT key, value FROM settings")
        .fetch_all(db)
        .await
        .map_err(|e| e.to_string())?;
    Ok(claude_path_from_rows(&rows))
}

async fn fetch_account(db: &Db, id: &str) -> Result<ClaudeAccount, String> {
    let row = sqlx::query(
        "SELECT id, label, config_dir, email, auth_method, api_provider, status, \
                is_default, last_checked_at, created_at \
         FROM claude_accounts WHERE id = ?",
    )
    .bind(id)
    .fetch_one(db)
    .await
    .map_err(|_| "Claude account not found".to_string())?;
    Ok(row_to_account(&row))
}

#[tauri::command]
pub async fn list_claude_accounts(db: State<'_, Db>) -> Result<Vec<ClaudeAccount>, String> {
    let rows = sqlx::query(
        "SELECT id, label, config_dir, email, auth_method, api_provider, status, \
                is_default, last_checked_at, created_at \
         FROM claude_accounts ORDER BY is_default DESC, label COLLATE NOCASE ASC",
    )
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows.iter().map(row_to_account).collect())
}

#[tauri::command]
pub async fn create_claude_account(
    app: AppHandle,
    db: State<'_, Db>,
    label: String,
) -> Result<ClaudeAccount, String> {
    let label = clean_label(&label)?;
    let id = Uuid::new_v4().to_string();
    let config_dir = account_config_dir(&app, &id)?;
    std::fs::create_dir_all(&config_dir).map_err(|e| e.to_string())?;

    let now = chrono::Utc::now().to_rfc3339();
    let existing: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM claude_accounts")
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    let is_default = existing == 0;

    sqlx::query(
        "INSERT INTO claude_accounts \
         (id, label, config_dir, status, is_default, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&label)
    .bind(config_dir.to_string_lossy().as_ref())
    .bind(STATUS_UNKNOWN)
    .bind(if is_default { 1 } else { 0 })
    .bind(&now)
    .execute(db.inner())
    .await
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            format!("A Claude account labelled '{label}' already exists.")
        } else {
            e.to_string()
        }
    })?;

    fetch_account(db.inner(), &id).await
}

#[tauri::command]
pub async fn rename_claude_account(
    db: State<'_, Db>,
    id: String,
    label: String,
) -> Result<ClaudeAccount, String> {
    let label = clean_label(&label)?;
    sqlx::query("UPDATE claude_accounts SET label = ? WHERE id = ?")
        .bind(&label)
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| {
            if e.to_string().contains("UNIQUE") {
                format!("A Claude account labelled '{label}' already exists.")
            } else {
                e.to_string()
            }
        })?;
    fetch_account(db.inner(), &id).await
}

#[tauri::command]
pub async fn delete_claude_account(db: State<'_, Db>, id: String) -> Result<(), String> {
    let was_default: i64 =
        sqlx::query_scalar("SELECT is_default FROM claude_accounts WHERE id = ?")
            .bind(&id)
            .fetch_optional(db.inner())
            .await
            .map_err(|e| e.to_string())?
            .unwrap_or(0);

    sqlx::query("UPDATE projects SET claude_account_id = NULL WHERE claude_account_id = ?")
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query("UPDATE runs SET claude_account_id = NULL WHERE claude_account_id = ?")
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query("DELETE FROM claude_accounts WHERE id = ?")
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    if was_default != 0 {
        if let Some(next_id) =
            sqlx::query_scalar::<_, String>("SELECT id FROM claude_accounts ORDER BY created_at LIMIT 1")
                .fetch_optional(db.inner())
                .await
                .map_err(|e| e.to_string())?
        {
            let _ = sqlx::query("UPDATE claude_accounts SET is_default = 1 WHERE id = ?")
                .bind(next_id)
                .execute(db.inner())
                .await;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn set_default_claude_account(db: State<'_, Db>, id: String) -> Result<(), String> {
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM claude_accounts WHERE id = ?")
        .bind(&id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    if exists == 0 {
        return Err("Claude account not found".to_string());
    }
    let mut tx = db.inner().begin().await.map_err(|e| e.to_string())?;
    sqlx::query("UPDATE claude_accounts SET is_default = 0")
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query("UPDATE claude_accounts SET is_default = 1 WHERE id = ?")
        .bind(&id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn set_project_claude_account(
    db: State<'_, Db>,
    project_id: String,
    account_id: Option<String>,
) -> Result<(), String> {
    let account_id = account_id.and_then(|v| {
        let v = v.trim().to_string();
        (!v.is_empty()).then_some(v)
    });
    sqlx::query("UPDATE projects SET claude_account_id = ? WHERE id = ?")
        .bind(account_id)
        .bind(project_id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn open_claude_account_login(
    db: State<'_, Db>,
    id: String,
    terminal_app: Option<String>,
) -> Result<(), String> {
    let account = fetch_account(db.inner(), &id).await?;
    std::fs::create_dir_all(&account.config_dir).map_err(|e| e.to_string())?;
    let claude_bin = load_claude_path(db.inner()).await?;
    let cwd = std::env::var("HOME").unwrap_or_else(|_| account.config_dir.clone());
    let login_cmd = format!(
        "cd {} && export CLAUDE_CONFIG_DIR={} && unset ANTHROPIC_API_KEY ANTHROPIC_AUTH_TOKEN CLAUDE_CODE_OAUTH_TOKEN ANTHROPIC_PROFILE && {} auth login --claudeai",
        shell_quote(&cwd),
        shell_quote(&account.config_dir),
        shell_quote(&claude_bin),
    );

    #[cfg(target_os = "macos")]
    {
        let terminal = terminal_app.unwrap_or_else(|| "terminal".to_string());
        let command = applescript_string(&login_cmd);
        let script = if terminal == "iterm" {
            format!(
                "tell application \"iTerm\"\n  create window with default profile\n  tell current session of current window to write text \"{}\"\n  activate\nend tell",
                command
            )
        } else {
            format!(
                "tell application \"Terminal\"\n  do script \"{}\"\n  activate\nend tell",
                command
            )
        };
        StdCommand::new("osascript")
            .arg("-e")
            .arg(script)
            .spawn()
            .map_err(|e| format!("Failed to open terminal for Claude login: {e}"))?;
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = terminal_app;
        Err(format!(
            "Open login in terminal is only supported on macOS. Run manually: {login_cmd}"
        ))
    }
}

#[tauri::command]
pub async fn validate_claude_account(
    db: State<'_, Db>,
    id: String,
) -> Result<ClaudeValidation, String> {
    validate_claude_account_inner(db.inner(), &id).await
}

pub async fn validate_claude_account_inner(
    db: &Db,
    id: &str,
) -> Result<ClaudeValidation, String> {
    let account = fetch_account(db, id).await?;
    std::fs::create_dir_all(&account.config_dir).map_err(|e| e.to_string())?;
    let claude_bin = load_claude_path(db).await?;

    let mut cmd = Command::new(&claude_bin);
    cmd.arg("auth")
        .arg("status")
        .arg("--json")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    augment_command_path(&mut cmd);
    apply_claude_config_dir(&mut cmd, Some(&account.config_dir));

    let output = tokio::time::timeout(Duration::from_secs(20), cmd.output())
        .await
        .map_err(|_| "Claude auth status timed out".to_string())?
        .map_err(|e| format!("Failed to run Claude auth status: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let parsed = serde_json::from_str::<Value>(&stdout).map_err(|_| {
        let detail = if stderr.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            stderr.trim().to_string()
        };
        format!("Claude auth status did not return JSON: {detail}")
    })?;

    let logged_in = parsed
        .get("loggedIn")
        .or_else(|| parsed.get("logged_in"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let auth_method = parsed
        .get("authMethod")
        .or_else(|| parsed.get("auth_method"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let api_provider = parsed
        .get("apiProvider")
        .or_else(|| parsed.get("api_provider"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let email = parsed
        .get("email")
        .or_else(|| parsed.get("accountEmail"))
        .or_else(|| parsed.get("userEmail"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string);
    let status = if logged_in {
        STATUS_READY
    } else {
        STATUS_NEEDS_LOGIN
    }
    .to_string();
    let checked_at = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        "UPDATE claude_accounts \
         SET email = ?, auth_method = ?, api_provider = ?, status = ?, last_checked_at = ? \
         WHERE id = ?",
    )
    .bind(&email)
    .bind(&auth_method)
    .bind(&api_provider)
    .bind(&status)
    .bind(&checked_at)
    .bind(id)
    .execute(db)
    .await
    .map_err(|e| e.to_string())?;

    if !output.status.success() && logged_in {
        sqlx::query("UPDATE claude_accounts SET status = ? WHERE id = ?")
            .bind(STATUS_ERROR)
            .bind(id)
            .execute(db)
            .await
            .ok();
    }

    Ok(ClaudeValidation {
        logged_in,
        status,
        email,
        auth_method,
        api_provider,
    })
}

/// Resolve the Claude account that should be used for a runtime operation.
///
/// Priority:
/// 1. explicit account id (used by resume);
/// 2. project-linked account;
/// 3. default Claude account;
/// 4. None, preserving the legacy global `~/.claude` profile behavior.
pub async fn resolve_runtime_account(
    db: &Db,
    project_id: Option<&str>,
    explicit_account_id: Option<&str>,
) -> Result<Option<ClaudeRuntimeAccount>, String> {
    let explicit = explicit_account_id.and_then(|v| {
        let v = v.trim();
        (!v.is_empty()).then_some(v.to_string())
    });

    let account_id = if let Some(id) = explicit {
        Some(id)
    } else if let Some(project_id) = project_id.filter(|v| !v.trim().is_empty()) {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT claude_account_id FROM projects WHERE id = ?",
        )
        .bind(project_id)
        .fetch_optional(db)
        .await
        .map_err(|e| e.to_string())?
        .flatten()
    } else {
        None
    };

    let account_id = match account_id {
        Some(id) if !id.trim().is_empty() => Some(id),
        _ => sqlx::query_scalar::<_, String>(
            "SELECT id FROM claude_accounts WHERE is_default = 1 ORDER BY created_at LIMIT 1",
        )
        .fetch_optional(db)
        .await
        .map_err(|e| e.to_string())?,
    };

    let Some(account_id) = account_id else {
        return Ok(None);
    };

    let row = sqlx::query("SELECT id, label, config_dir FROM claude_accounts WHERE id = ?")
        .bind(&account_id)
        .fetch_optional(db)
        .await
        .map_err(|e| e.to_string())?;
    let Some(row) = row else {
        return if explicit_account_id.is_some() {
            Err("Claude account saved on this run no longer exists".to_string())
        } else {
            Ok(None)
        };
    };

    let config_dir: String = row.get("config_dir");
    if config_dir.trim().is_empty() {
        return Err("Claude account has no config directory".to_string());
    }
    std::fs::create_dir_all(Path::new(&config_dir)).map_err(|e| e.to_string())?;

    Ok(Some(ClaudeRuntimeAccount {
        id: row.get("id"),
        label: row.get("label"),
        config_dir,
    }))
}

pub async fn default_runtime_account(db: &Db) -> Result<Option<ClaudeRuntimeAccount>, String> {
    resolve_runtime_account(db, None, None).await
}

/// Id of the current default Claude account, if any. Lightweight (no filesystem
/// side effects) — used by the read-only budget/usage badge paths to pick which
/// per-account plan-usage snapshot to display.
pub async fn default_account_id(db: &Db) -> Option<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT id FROM claude_accounts WHERE is_default = 1 ORDER BY created_at LIMIT 1",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
}

/// The Claude account id snapshotted on a run (`None` for legacy/global runs).
pub async fn run_account_id(db: &Db, run_id: &str) -> Option<String> {
    sqlx::query_scalar::<_, Option<String>>("SELECT claude_account_id FROM runs WHERE id = ?")
        .bind(run_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .flatten()
}

/// The Claude account id linked to a project (`None` when unlinked).
pub async fn project_account_id(db: &Db, project_id: &str) -> Option<String> {
    sqlx::query_scalar::<_, Option<String>>("SELECT claude_account_id FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .flatten()
}

/// Absolute config dirs of every Devdy-managed Claude account. Used by session
/// mirroring and storage cleanup to scan transcripts across account profiles in
/// addition to the global `~/.claude` store. Best-effort: returns empty on error.
pub async fn account_config_dirs(db: &Db) -> Vec<String> {
    sqlx::query_scalar::<_, String>("SELECT config_dir FROM claude_accounts")
        .fetch_all(db)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|d| !d.trim().is_empty())
        .collect()
}
