//! Central VPS/server management (parallel to github_accounts / mcp).
//!
//! A managed server is defined once (label, host, port, user, auth method) and
//! its SSH private-key passphrase — if any — lives in the macOS Keychain
//! (consolidated store, map `servers`). SQLite never holds the passphrase; the
//! frontend only ever receives a `has_passphrase` boolean. Connection testing
//! runs SSH non-interactively (`BatchMode=yes` + `ConnectTimeout`) so it never
//! prompts and never leaks the secret through argv.

use crate::db::Db;
use crate::secrets;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::time::Duration;
use tauri::State;
use uuid::Uuid;

// ---- Wire types ------------------------------------------------------------

/// Server summary for list/detail views. Never carries the passphrase VALUE —
/// only the derived `has_passphrase` flag.
#[derive(Debug, Serialize, Clone)]
pub struct VpsServer {
    pub id: String,
    pub label: String,
    pub host: String,
    pub port: i64,
    pub username: String,
    pub auth_method: String, // 'agent' | 'key'
    pub private_key_path: Option<String>,
    pub tags: Option<String>,
    pub status: Option<String>, // 'online' | 'offline' | 'unknown' | None
    pub last_checked_at: Option<String>,
    pub has_passphrase: bool, // derived from Keychain, never the value
    pub created_at: String,
}

// No `Debug` derive: this payload holds a secret (`passphrase`), so we never
// want it accidentally rendered via `{:?}` in a log/error (SEC-001).
#[derive(Deserialize)]
pub struct CreateServerPayload {
    pub label: String,
    pub host: String,
    #[serde(default)]
    pub port: Option<i64>, // default 22 when None
    pub username: String,
    pub auth_method: String,
    #[serde(default)]
    pub private_key_path: Option<String>,
    #[serde(default)]
    pub tags: Option<String>,
    #[serde(default)]
    pub passphrase: Option<String>, // Some(v) → store in Keychain; None → skip
}

// No `Debug` derive: holds a secret (`passphrase`) — see CreateServerPayload (SEC-001).
#[derive(Deserialize)]
pub struct UpdateServerPayload {
    pub id: String,
    pub label: String,
    pub host: String,
    #[serde(default)]
    pub port: Option<i64>,
    pub username: String,
    pub auth_method: String,
    #[serde(default)]
    pub private_key_path: Option<String>,
    #[serde(default)]
    pub tags: Option<String>,
    #[serde(default)]
    pub passphrase: Option<String>, // None/"" → keep; Some(non-empty) → overwrite
}

/// Result of a connection test (shape matches `mcp::TestConnectionResult`).
#[derive(Debug, Serialize)]
pub struct TestConnectionResult {
    pub ok: bool,
    pub message: String,
}

// ---- Validation ------------------------------------------------------------

/// Validated, normalized fields ready for persistence.
struct ValidatedFields {
    port: i64,
    auth_method: String,
    /// `Some(path)` only for auth_method == "key"; `None` (→ NULL) for "agent".
    private_key_path: Option<String>,
}

/// Validate BR-001..003 BEFORE any DB write. Returns cleaned fields on success,
/// `Err(String)` otherwise (never writes the DB when it fails).
fn validate_payload(
    label: &str,
    host: &str,
    username: &str,
    port: Option<i64>,
    auth_method: &str,
    private_key_path: &Option<String>,
) -> Result<ValidatedFields, String> {
    // BR-003: label/host/username non-empty after trim.
    if label.trim().is_empty() {
        return Err("label cannot be empty".to_string());
    }
    if host.trim().is_empty() {
        return Err("host cannot be empty".to_string());
    }
    if username.trim().is_empty() {
        return Err("username cannot be empty".to_string());
    }

    // BR-003: port defaults to 22, must be 1..=65535.
    let port = port.unwrap_or(22);
    if !(1..=65535).contains(&port) {
        return Err(format!("port must be between 1 and 65535 (got {})", port));
    }

    // BR-001: auth_method ∈ {agent, key}.
    match auth_method {
        "agent" => Ok(ValidatedFields {
            port,
            auth_method: auth_method.to_string(),
            private_key_path: None, // 'agent' ignores the key path → NULL
        }),
        "key" => {
            // BR-002: 'key' requires a non-empty private_key_path.
            let path = private_key_path
                .as_ref()
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty());
            match path {
                Some(p) => Ok(ValidatedFields {
                    port,
                    auth_method: auth_method.to_string(),
                    private_key_path: Some(p),
                }),
                None => Err("auth_method 'key' requires a private_key_path".to_string()),
            }
        }
        other => Err(format!(
            "auth_method must be 'agent' or 'key' (got '{}')",
            other
        )),
    }
}

// ---- Row mapping -----------------------------------------------------------

const SELECT_COLS: &str = "id, label, host, port, username, auth_method, private_key_path, \
     tags, status, last_checked_at, created_at";

fn row_to_server(row: &sqlx::sqlite::SqliteRow) -> VpsServer {
    let id: String = row.get("id");
    let has_passphrase = secrets::has_server_secret(&id);
    VpsServer {
        label: row.get("label"),
        host: row.get("host"),
        port: row.get("port"),
        username: row.get("username"),
        auth_method: row.get("auth_method"),
        private_key_path: row.get("private_key_path"),
        tags: row.get("tags"),
        status: row.get("status"),
        last_checked_at: row.get("last_checked_at"),
        has_passphrase,
        created_at: row.get("created_at"),
        id,
    }
}

async fn fetch_server(db: &Db, id: &str) -> Result<VpsServer, String> {
    let row = sqlx::query(&format!("SELECT {} FROM servers WHERE id = ?", SELECT_COLS))
        .bind(id)
        .fetch_one(db)
        .await
        .map_err(|e| format!("Server not found: {}", e))?;
    Ok(row_to_server(&row))
}

// ---- CRUD commands ---------------------------------------------------------

#[tauri::command]
pub async fn list_vps_servers(db: State<'_, Db>) -> Result<Vec<VpsServer>, String> {
    let rows = sqlx::query(&format!("SELECT {} FROM servers ORDER BY label", SELECT_COLS))
        .fetch_all(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(rows.iter().map(row_to_server).collect())
}

#[tauri::command]
pub async fn create_vps_server(
    db: State<'_, Db>,
    payload: CreateServerPayload,
) -> Result<VpsServer, String> {
    // Validate BEFORE any DB write (BR-001..003).
    let fields = validate_payload(
        &payload.label,
        &payload.host,
        &payload.username,
        payload.port,
        &payload.auth_method,
        &payload.private_key_path,
    )?;

    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        "INSERT INTO servers (id, label, host, port, username, auth_method, private_key_path, \
         tags, status, last_checked_at, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, ?)",
    )
    .bind(&id)
    .bind(payload.label.trim())
    .bind(payload.host.trim())
    .bind(fields.port)
    .bind(payload.username.trim())
    .bind(&fields.auth_method)
    .bind(&fields.private_key_path)
    .bind(&payload.tags)
    .bind(&now)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    // Store the passphrase only when a non-empty one is supplied.
    if let Some(pass) = payload.passphrase.as_ref().filter(|p| !p.is_empty()) {
        secrets::set_server_secret(&id, pass)
            .map_err(|e| format!("Failed to store passphrase: {}", e))?;
    }

    fetch_server(db.inner(), &id).await
}

#[tauri::command]
pub async fn update_vps_server(
    db: State<'_, Db>,
    payload: UpdateServerPayload,
) -> Result<VpsServer, String> {
    // Validate BEFORE any DB write (BR-001..003).
    let fields = validate_payload(
        &payload.label,
        &payload.host,
        &payload.username,
        payload.port,
        &payload.auth_method,
        &payload.private_key_path,
    )?;

    // Ensure the server exists.
    let _ = sqlx::query("SELECT id FROM servers WHERE id = ?")
        .bind(&payload.id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| format!("Server not found: {}", e))?;

    sqlx::query(
        "UPDATE servers SET label = ?, host = ?, port = ?, username = ?, auth_method = ?, \
         private_key_path = ?, tags = ? WHERE id = ?",
    )
    .bind(payload.label.trim())
    .bind(payload.host.trim())
    .bind(fields.port)
    .bind(payload.username.trim())
    .bind(&fields.auth_method)
    .bind(&fields.private_key_path)
    .bind(&payload.tags)
    .bind(&payload.id)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    // BR-005 / AC-008: a new non-empty passphrase overwrites; None/"" keeps the
    // stored value untouched.
    if let Some(pass) = payload.passphrase.as_ref().filter(|p| !p.is_empty()) {
        secrets::set_server_secret(&payload.id, pass)
            .map_err(|e| format!("Failed to store passphrase: {}", e))?;
    }

    fetch_server(db.inner(), &payload.id).await
}

#[tauri::command]
pub async fn delete_vps_server(db: State<'_, Db>, id: String) -> Result<(), String> {
    // BR-004: remove the Keychain secret alongside the row.
    let _ = secrets::delete_server_secret(&id);
    sqlx::query("DELETE FROM servers WHERE id = ?")
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ---- Test connection -------------------------------------------------------

/// Overall wrapper timeout. ConnectTimeout=10 governs the TCP/SSH connect; the
/// extra headroom covers process spawn + auth so a dead host never hangs the UI.
const SSH_TIMEOUT: Duration = Duration::from_secs(12);

/// SEC-002 / BR-006: run SSH non-interactively and report reachability.
///
/// Command: `ssh -o BatchMode=yes -o ConnectTimeout=10 -p <port> [-i <key>]
/// <user>@<host> echo devdy-ok`. `ok` requires exit 0 AND stdout containing
/// `devdy-ok`. The result (`online`/`offline`) plus `last_checked_at` is written
/// back to the row. Never logs the passphrase or raw ssh stdout.
#[tauri::command]
pub async fn test_vps_connection(
    db: State<'_, Db>,
    id: String,
) -> Result<TestConnectionResult, String> {
    let server = fetch_server(db.inner(), &id).await?;

    let result = run_ssh_probe(&server).await;

    let status = if result.ok { "online" } else { "offline" };
    let now = chrono::Utc::now().to_rfc3339();
    let _ = sqlx::query("UPDATE servers SET status = ?, last_checked_at = ? WHERE id = ?")
        .bind(status)
        .bind(&now)
        .bind(&id)
        .execute(db.inner())
        .await;

    Ok(result)
}

/// Spawn the non-interactive SSH probe under a hard timeout. Returns a concise
/// `TestConnectionResult`; deliberately does NOT include raw stdout/stderr in
/// the message so no server-side output can leak here.
async fn run_ssh_probe(server: &VpsServer) -> TestConnectionResult {
    use tokio::process::Command;

    let mut cmd = Command::new("ssh");
    cmd.arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=10")
        .arg("-p")
        .arg(server.port.to_string());
    // Only pass an identity file for key auth (agent auth relies on ssh-agent).
    if server.auth_method == "key" {
        if let Some(path) = server.private_key_path.as_ref().filter(|p| !p.is_empty()) {
            cmd.arg("-i").arg(path);
        }
    }
    cmd.arg(format!("{}@{}", server.username, server.host))
        .arg("echo")
        .arg("devdy-ok");
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let output = match tokio::time::timeout(SSH_TIMEOUT, cmd.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return TestConnectionResult {
                ok: false,
                message: format!("Failed to run ssh: {}", e),
            };
        }
        Err(_) => {
            return TestConnectionResult {
                ok: false,
                message: format!("Timed out after {}s", SSH_TIMEOUT.as_secs()),
            };
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.status.success() && stdout.contains("devdy-ok") {
        TestConnectionResult {
            ok: true,
            message: "Connection succeeded".to_string(),
        }
    } else {
        TestConnectionResult {
            ok: false,
            message: "Connection failed or host unreachable".to_string(),
        }
    }
}

// ---- Per-project assignment (GĐ2) ------------------------------------------
//
// Maps a managed server to a project under a deployment role. Mirrors the
// `project_mcp_servers` precedent. SEC-101: never carries the passphrase VALUE —
// only the derived `has_passphrase` flag, exactly like `VpsServer`.

/// A server mapped to a project, carrying the full server summary plus the
/// deployment `role`. Never includes the passphrase (SEC-101) — only
/// `has_passphrase`.
#[derive(Debug, Serialize, Clone)]
pub struct ProjectServer {
    pub id: String,
    pub label: String,
    pub host: String,
    pub port: i64,
    pub username: String,
    pub auth_method: String,
    pub private_key_path: Option<String>,
    pub tags: Option<String>,
    pub status: Option<String>,
    pub last_checked_at: Option<String>,
    pub has_passphrase: bool,
    pub created_at: String,
    pub role: String,
}

fn row_to_project_server(row: &sqlx::sqlite::SqliteRow) -> ProjectServer {
    let id: String = row.get("id");
    let has_passphrase = secrets::has_server_secret(&id);
    ProjectServer {
        label: row.get("label"),
        host: row.get("host"),
        port: row.get("port"),
        username: row.get("username"),
        auth_method: row.get("auth_method"),
        private_key_path: row.get("private_key_path"),
        tags: row.get("tags"),
        status: row.get("status"),
        last_checked_at: row.get("last_checked_at"),
        has_passphrase,
        created_at: row.get("created_at"),
        role: row.get("role"),
        id,
    }
}

/// FR-101 / AC-101,102: list every server mapped to `project_id`. JOIN
/// `project_servers` × `servers`, ordered by role then label. A server mapped
/// under two roles yields two rows. Empty project → `[]` (no error).
#[tauri::command]
pub async fn list_project_servers(
    db: State<'_, Db>,
    project_id: String,
) -> Result<Vec<ProjectServer>, String> {
    let rows = sqlx::query(&format!(
        "SELECT {}, ps.role AS role FROM project_servers ps \
         JOIN servers ON servers.id = ps.server_id \
         WHERE ps.project_id = ? ORDER BY ps.role, servers.label",
        SELECT_COLS
    ))
    .bind(&project_id)
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows.iter().map(row_to_project_server).collect())
}

/// FR-102 / AC-103,104,105: map a server to a project under a role.
/// BR-101: role is trimmed; empty → `'production'`. BR-102: idempotent via
/// `INSERT OR IGNORE` on the (project, server, role) PK. BR-103: FK guarantees
/// both ids exist — a violation returns `Err`, never panics.
#[tauri::command]
pub async fn map_server_to_project(
    db: State<'_, Db>,
    project_id: String,
    server_id: String,
    role: String,
) -> Result<(), String> {
    let role = role.trim();
    let role = if role.is_empty() { "production" } else { role };
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT OR IGNORE INTO project_servers (project_id, server_id, role, created_at) \
         VALUES (?, ?, ?, ?)",
    )
    .bind(&project_id)
    .bind(&server_id)
    .bind(role)
    .bind(&now)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// FR-103 / AC-106 / BR-104: remove exactly the (project, server, role) triple.
/// Other mappings of the same server/project are untouched.
#[tauri::command]
pub async fn unmap_server(
    db: State<'_, Db>,
    project_id: String,
    server_id: String,
    role: String,
) -> Result<(), String> {
    sqlx::query(
        "DELETE FROM project_servers WHERE project_id = ? AND server_id = ? AND role = ?",
    )
    .bind(&project_id)
    .bind(&server_id)
    .bind(&role)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}
