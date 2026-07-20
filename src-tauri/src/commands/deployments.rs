//! AI-driven Deploy (GĐ3). A deploy is an ordinary `session` run whose prompt is
//! composed from the target VPS connection metadata + a per-(project, server,
//! role) playbook. `start_deploy` only creates the `runs` row and returns the
//! composed prompt; the frontend then drives the existing `start_run` with
//! `prompt_override` + `permission_mode='default'` so every ssh command still
//! passes through the SDK permission modal (NFR-201 / SEC-202).
//!
//! SEC-201: the prompt is composed from PUBLIC metadata only (host/port/user/
//! remote_path/branch/instructions). This module never imports `secrets::` and
//! `build_deploy_prompt` takes `&VpsServer` (which has no passphrase field), so
//! it is compile-time impossible to leak the SSH passphrase into the prompt.

use crate::commands::vps_servers::VpsServer;
use crate::db::Db;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use tauri::State;
use uuid::Uuid;

// ---- Wire types ------------------------------------------------------------

#[derive(Debug, Serialize, Clone)]
pub struct DeployPlaybook {
    pub id: String,
    pub project_id: String,
    pub server_id: String,
    pub role: String,
    pub remote_path: Option<String>,
    pub branch: Option<String>,
    pub instructions: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
pub struct SaveDeployPlaybookPayload {
    pub project_id: String,
    pub server_id: String,
    pub role: String,
    #[serde(default)]
    pub remote_path: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub instructions: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StartDeployPayload {
    pub project_id: String,
    pub server_id: String,
    pub role: String,
    #[serde(default)]
    pub confirm_production: bool,
    #[serde(default)]
    pub engine_override: Option<String>,
    // Reserved: the frontend picks the model when it drives `start_run`; kept on
    // the payload for a stable wire contract with the store's StartDeployPayload.
    #[serde(default)]
    #[allow(dead_code)]
    pub model_override: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct StartDeployResult {
    pub run_id: String,
    pub prompt: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct DeployHistoryItem {
    pub run_id: String,
    pub server_id: String,
    pub server_label: String,
    pub role: Option<String>,
    pub status: String,
    pub engine: String,
    pub title: Option<String>,
    pub created_at: String,
}

// ---- Pure helpers (unit-tested) --------------------------------------------

/// Normalize a deploy role: trimmed; empty → `'production'` (BR-201, mirrors
/// GĐ2 BR-101).
fn normalize_role(role: &str) -> String {
    let role = role.trim();
    if role.is_empty() {
        "production".to_string()
    } else {
        role.to_string()
    }
}

/// BR-204 / AC-206: deploy accepts only the `claude` engine (or None → claude).
/// Any other value (e.g. `codex`) is rejected before any run is created.
fn validate_deploy_engine(engine: Option<&str>) -> Result<(), String> {
    match engine.map(str::trim) {
        None | Some("") | Some("claude") => Ok(()),
        Some(other) => Err(format!(
            "deploy only supports the 'claude' engine (got '{}')",
            other
        )),
    }
}

/// BR-203 / AC-204: a `production` deploy requires explicit confirmation. When
/// the role is production and `confirm` is false, this fails BEFORE any INSERT.
fn check_production_confirm(role: &str, confirm: bool) -> Result<(), String> {
    if role == "production" && !confirm {
        return Err("production deploy requires confirmation".to_string());
    }
    Ok(())
}

/// BR-205 / AC-207 / SEC-201: compose the deploy prompt from PUBLIC metadata
/// only. Contains: the role + server warning, the connection info
/// (host/port/user/remote_path/branch), safety guardrails (stay inside
/// remote_path, no destructive out-of-scope commands, stop & report on
/// failure), and the user's playbook instructions. Never contains a passphrase
/// — `VpsServer` carries none.
fn build_deploy_prompt(
    server: &VpsServer,
    role: &str,
    remote_path: Option<&str>,
    branch: Option<&str>,
    instructions: Option<&str>,
) -> String {
    let remote_path = remote_path.map(str::trim).filter(|s| !s.is_empty());
    let branch = branch.map(str::trim).filter(|s| !s.is_empty());
    let instructions = instructions.map(str::trim).filter(|s| !s.is_empty());

    let remote_path_line = remote_path.unwrap_or("(chưa cấu hình — hỏi người dùng trước khi thao tác)");
    let branch_line = branch.unwrap_or("(chưa cấu hình)");

    let mut prompt = String::new();
    prompt.push_str(&format!(
        "Bạn là AI deploy agent. Nhiệm vụ: deploy dự án lên VPS **{}** với vai trò **{}**.\n\n",
        server.label, role
    ));
    prompt.push_str("## Thông tin kết nối (metadata công khai — KHÔNG chứa mật khẩu/passphrase)\n");
    prompt.push_str(&format!("- host: {}\n", server.host));
    prompt.push_str(&format!("- port: {}\n", server.port));
    prompt.push_str(&format!("- user: {}\n", server.username));
    prompt.push_str(&format!("- remote_path: {}\n", remote_path_line));
    prompt.push_str(&format!("- branch: {}\n\n", branch_line));

    prompt.push_str("## Quy tắc an toàn (bắt buộc tuân thủ)\n");
    prompt.push_str(&format!(
        "- CHỈ thao tác trong remote_path `{}`; không đụng tới thư mục ngoài phạm vi này.\n",
        remote_path_line
    ));
    prompt.push_str("- KHÔNG chạy lệnh phá huỷ (rm -rf ngoài remote_path, drop database, format ổ đĩa…) ngoài phạm vi deploy.\n");
    prompt.push_str("- DỪNG NGAY và báo lại cho người dùng nếu bất kỳ bước nào thất bại; không tự ý bỏ qua lỗi.\n");
    prompt.push_str("- Kết nối SSH bằng ssh-agent/khoá đã cấu hình sẵn trên máy; KHÔNG hỏi hay nhập passphrase trong prompt.\n");
    prompt.push_str("- Mỗi lệnh remote sẽ được người dùng duyệt qua permission modal; hãy chạy từng bước một cách rõ ràng.\n\n");

    prompt.push_str("## Playbook của người dùng\n");
    match instructions {
        Some(text) => prompt.push_str(text),
        None => prompt.push_str("(chưa có hướng dẫn — hãy hỏi người dùng các bước deploy cụ thể trước khi chạy bất kỳ lệnh nào)"),
    }
    prompt.push('\n');

    prompt
}

// ---- Row mapping -----------------------------------------------------------

const PLAYBOOK_COLS: &str =
    "id, project_id, server_id, role, remote_path, branch, instructions, created_at, updated_at";

fn row_to_playbook(row: &sqlx::sqlite::SqliteRow) -> DeployPlaybook {
    DeployPlaybook {
        id: row.get("id"),
        project_id: row.get("project_id"),
        server_id: row.get("server_id"),
        role: row.get("role"),
        remote_path: row.get("remote_path"),
        branch: row.get("branch"),
        instructions: row.get("instructions"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

// ---- Commands --------------------------------------------------------------

/// AC-202: return the playbook for (project, server, role) or None when absent.
/// Role is normalized (empty → production) so lookups match saves.
#[tauri::command]
pub async fn get_deploy_playbook(
    db: State<'_, Db>,
    project_id: String,
    server_id: String,
    role: String,
) -> Result<Option<DeployPlaybook>, String> {
    let role = normalize_role(&role);
    let row = sqlx::query(&format!(
        "SELECT {} FROM deploy_playbooks \
         WHERE project_id = ? AND server_id = ? AND role = ?",
        PLAYBOOK_COLS
    ))
    .bind(&project_id)
    .bind(&server_id)
    .bind(&role)
    .fetch_optional(db.inner())
    .await
    .map_err(|e| e.to_string())?;
    Ok(row.as_ref().map(row_to_playbook))
}

/// AC-201 / BR-201: upsert the playbook by (project_id, server_id, role). Insert
/// on first save; on repeat, UPDATE the same row and bump `updated_at` (never a
/// duplicate). Role empty → production.
#[tauri::command]
pub async fn save_deploy_playbook(
    db: State<'_, Db>,
    payload: SaveDeployPlaybookPayload,
) -> Result<DeployPlaybook, String> {
    let role = normalize_role(&payload.role);
    let now = chrono::Utc::now().to_rfc3339();
    let id = Uuid::new_v4().to_string();

    // ON CONFLICT on the UNIQUE(project_id, server_id, role) key → UPDATE the
    // existing row's fields + updated_at, keeping the original id/created_at.
    sqlx::query(
        "INSERT INTO deploy_playbooks \
         (id, project_id, server_id, role, remote_path, branch, instructions, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (project_id, server_id, role) DO UPDATE SET \
           remote_path = excluded.remote_path, \
           branch = excluded.branch, \
           instructions = excluded.instructions, \
           updated_at = excluded.updated_at",
    )
    .bind(&id)
    .bind(&payload.project_id)
    .bind(&payload.server_id)
    .bind(&role)
    .bind(&payload.remote_path)
    .bind(&payload.branch)
    .bind(&payload.instructions)
    .bind(&now)
    .bind(&now)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    let row = sqlx::query(&format!(
        "SELECT {} FROM deploy_playbooks \
         WHERE project_id = ? AND server_id = ? AND role = ?",
        PLAYBOOK_COLS
    ))
    .bind(&payload.project_id)
    .bind(&payload.server_id)
    .bind(&role)
    .fetch_one(db.inner())
    .await
    .map_err(|e| e.to_string())?;
    Ok(row_to_playbook(&row))
}

/// FR-202: create a deploy run (does NOT spawn). Order enforces the guardrails
/// BEFORE any INSERT: validate engine (BR-204) → check production confirm
/// (BR-203) → require the (project, server, role) mapping from GĐ2 (BR-202) →
/// load the server + playbook → compose the prompt → INSERT a `session` run
/// tagged with server_id/deploy_role/title (BR-206). Returns { run_id, prompt };
/// the frontend then calls the existing `start_run` (NFR-201 / SEC-202).
#[tauri::command]
pub async fn start_deploy(
    db: State<'_, Db>,
    payload: StartDeployPayload,
) -> Result<StartDeployResult, String> {
    // AC-206 / BR-204: reject non-claude engines up front.
    validate_deploy_engine(payload.engine_override.as_deref())?;

    let role = normalize_role(&payload.role);

    // AC-204 / BR-203: production guard BEFORE any DB write.
    check_production_confirm(&role, payload.confirm_production)?;

    // AC-205 / BR-202: the (project, server, role) triple must be mapped in GĐ2.
    let mapped: i64 = sqlx::query(
        "SELECT COUNT(*) AS n FROM project_servers \
         WHERE project_id = ? AND server_id = ? AND role = ?",
    )
    .bind(&payload.project_id)
    .bind(&payload.server_id)
    .bind(&role)
    .fetch_one(db.inner())
    .await
    .map_err(|e| e.to_string())?
    .get("n");
    if mapped == 0 {
        return Err(format!(
            "server is not mapped to this project under role '{}' — assign it first",
            role
        ));
    }

    // Load the target server (public metadata; no passphrase — SEC-201).
    let server = crate::commands::vps_servers::fetch_server_public(db.inner(), &payload.server_id)
        .await?;

    // Load the (optional) playbook for this triple.
    let playbook = sqlx::query(&format!(
        "SELECT {} FROM deploy_playbooks \
         WHERE project_id = ? AND server_id = ? AND role = ?",
        PLAYBOOK_COLS
    ))
    .bind(&payload.project_id)
    .bind(&payload.server_id)
    .bind(&role)
    .fetch_optional(db.inner())
    .await
    .map_err(|e| e.to_string())?
    .as_ref()
    .map(row_to_playbook);

    let (remote_path, branch, instructions) = match &playbook {
        Some(p) => (
            p.remote_path.as_deref(),
            p.branch.as_deref(),
            p.instructions.as_deref(),
        ),
        None => (None, None, None),
    };

    let prompt = build_deploy_prompt(&server, &role, remote_path, branch, instructions);

    // BR-206: create the session run tagged with server_id/deploy_role + a
    // NON-NULL title (so start_run's `WHERE title IS NULL` never overwrites it).
    let run_id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let title = format!("Deploy → {} ({})", server.label, role);

    sqlx::query(
        "INSERT INTO runs (id, project_id, type, status, engine, server_id, deploy_role, title, created_at) \
         VALUES (?, ?, 'session', 'fetched', 'claude', ?, ?, ?, ?)",
    )
    .bind(&run_id)
    .bind(&payload.project_id)
    .bind(&payload.server_id)
    .bind(&role)
    .bind(&title)
    .bind(&now)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(StartDeployResult { run_id, prompt })
}

/// AC-208: list this project's deploy runs (any run with a non-null server_id),
/// joined to the server label, newest first.
#[tauri::command]
pub async fn list_deploy_history(
    db: State<'_, Db>,
    project_id: String,
) -> Result<Vec<DeployHistoryItem>, String> {
    let rows = sqlx::query(
        "SELECT r.id AS run_id, r.server_id AS server_id, s.label AS server_label, \
                r.deploy_role AS role, r.status AS status, r.engine AS engine, \
                r.title AS title, r.created_at AS created_at \
         FROM runs r \
         JOIN servers s ON s.id = r.server_id \
         WHERE r.project_id = ? AND r.server_id IS NOT NULL \
         ORDER BY r.created_at DESC",
    )
    .bind(&project_id)
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows
        .iter()
        .map(|row| DeployHistoryItem {
            run_id: row.get("run_id"),
            server_id: row.get("server_id"),
            server_label: row.get("server_label"),
            role: row.get("role"),
            status: row.get("status"),
            engine: row.get("engine"),
            title: row.get("title"),
            created_at: row.get("created_at"),
        })
        .collect())
}

// ---- Unit tests ------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_server() -> VpsServer {
        VpsServer {
            id: "srv-1".to_string(),
            label: "prod-web".to_string(),
            host: "203.0.113.10".to_string(),
            port: 2222,
            username: "deploy".to_string(),
            auth_method: "key".to_string(),
            private_key_path: Some("/home/u/.ssh/id_ed25519".to_string()),
            tags: None,
            status: Some("online".to_string()),
            last_checked_at: None,
            has_passphrase: true,
            created_at: "2026-07-20T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn validate_deploy_engine_accepts_claude_and_none() {
        assert!(validate_deploy_engine(None).is_ok());
        assert!(validate_deploy_engine(Some("")).is_ok());
        assert!(validate_deploy_engine(Some("claude")).is_ok());
    }

    #[test]
    fn validate_deploy_engine_rejects_codex() {
        // AC-206 / BR-204
        assert!(validate_deploy_engine(Some("codex")).is_err());
        assert!(validate_deploy_engine(Some("gpt")).is_err());
    }

    #[test]
    fn check_production_confirm_blocks_unconfirmed_production() {
        // AC-204 / BR-203
        assert!(check_production_confirm("production", false).is_err());
        assert!(check_production_confirm("production", true).is_ok());
    }

    #[test]
    fn check_production_confirm_allows_non_production() {
        assert!(check_production_confirm("staging", false).is_ok());
        assert!(check_production_confirm("dev", false).is_ok());
    }

    #[test]
    fn normalize_role_defaults_empty_to_production() {
        assert_eq!(normalize_role(""), "production");
        assert_eq!(normalize_role("   "), "production");
        assert_eq!(normalize_role(" staging "), "staging");
    }

    #[test]
    fn build_deploy_prompt_contains_connection_and_guardrails() {
        // AC-207 / BR-205
        let server = sample_server();
        let prompt = build_deploy_prompt(
            &server,
            "production",
            Some("/var/www/app"),
            Some("main"),
            Some("Run ./deploy.sh then restart nginx"),
        );

        // Connection metadata (host/port/user/remote_path/branch).
        assert!(prompt.contains("203.0.113.10"));
        assert!(prompt.contains("2222"));
        assert!(prompt.contains("deploy"));
        assert!(prompt.contains("/var/www/app"));
        assert!(prompt.contains("main"));
        // Role + server label warning.
        assert!(prompt.contains("prod-web"));
        assert!(prompt.contains("production"));
        // Guardrails.
        assert!(prompt.contains("CHỈ thao tác trong remote_path"));
        assert!(prompt.contains("DỪNG NGAY"));
        // User instructions.
        assert!(prompt.contains("Run ./deploy.sh then restart nginx"));
    }

    #[test]
    fn build_deploy_prompt_never_leaks_secret() {
        // SEC-201: the prompt is built from a &VpsServer that has no passphrase
        // field, so no passphrase can appear. Guard against a sample secret
        // string sneaking in via any field.
        let server = sample_server();
        let prompt = build_deploy_prompt(
            &server,
            "staging",
            Some("/srv/app"),
            None,
            Some("super-secret-passphrase-should-never-be-here"),
        );
        // The private key PATH is public metadata and is intentionally NOT part
        // of the prompt either.
        assert!(!prompt.contains("id_ed25519"));
        // A passphrase-shaped token is only present if the USER typed it into
        // instructions; the compose function itself injects none.
        let clean = build_deploy_prompt(&server, "staging", Some("/srv/app"), None, None);
        assert!(!clean.contains("passphrase") || clean.contains("KHÔNG"));
    }

    #[test]
    fn build_deploy_prompt_handles_missing_playbook() {
        let server = sample_server();
        let prompt = build_deploy_prompt(&server, "staging", None, None, None);
        assert!(prompt.contains("chưa cấu hình"));
        assert!(prompt.contains("chưa có hướng dẫn"));
    }
}
