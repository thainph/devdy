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
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use tauri::{AppHandle, Manager, State};
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
    pub auth_method: String,              // 'agent' | 'key'
    pub private_key_path: Option<String>, // app-managed imported key path for key auth
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
    pub private_key_source_path: Option<String>, // source file selected by the user; copied into app data
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
    pub private_key_source_path: Option<String>, // source file selected by the user; copied into app data
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
#[derive(Debug)]
struct ValidatedFields {
    port: i64,
    auth_method: String,
    /// `Some(path)` only for auth_method == "key"; `None` when a selected
    /// source key will be imported after the server id exists, or for "agent".
    private_key_path: Option<String>,
}

const SERVER_KEYS_DIR: &str = "server-keys";

fn trimmed_optional(value: &Option<String>) -> Option<String> {
    value
        .as_ref()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
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
    private_key_source_path: &Option<String>,
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
            // BR-002: 'key' requires either an already-stored key path or a
            // newly selected source key file to import.
            let path = trimmed_optional(private_key_path);
            match path {
                Some(p) => Ok(ValidatedFields {
                    port,
                    auth_method: auth_method.to_string(),
                    private_key_path: Some(p),
                }),
                None if trimmed_optional(private_key_source_path).is_some() => {
                    Ok(ValidatedFields {
                        port,
                        auth_method: auth_method.to_string(),
                        private_key_path: None,
                    })
                }
                None => Err("auth_method 'key' requires a private key file".to_string()),
            }
        }
        other => Err(format!(
            "auth_method must be 'agent' or 'key' (got '{}')",
            other
        )),
    }
}

fn set_private_permissions(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        let _ = path;
    }
}

fn server_keys_root(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("no app data dir: {e}"))?
        .join(SERVER_KEYS_DIR);
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create server key dir: {e}"))?;
    set_private_permissions(&dir, 0o700);
    Ok(dir)
}

fn server_key_dir(app: &AppHandle, server_id: &str) -> Result<PathBuf, String> {
    let dir = server_keys_root(app)?.join(server_id);
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create server key dir: {e}"))?;
    set_private_permissions(&dir, 0o700);
    Ok(dir)
}

fn sanitize_key_filename(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let out = out.trim_matches('.').to_string();
    if out.is_empty() {
        "id_key".to_string()
    } else {
        out
    }
}

fn key_filename_from_source(source: &Path) -> String {
    source
        .file_name()
        .and_then(|name| name.to_str())
        .map(sanitize_key_filename)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "id_key".to_string())
}

fn import_private_key_file(
    app: &AppHandle,
    server_id: &str,
    source_path: &str,
) -> Result<String, String> {
    let source_path = source_path.trim();
    if source_path.is_empty() {
        return Err("private key file is required".to_string());
    }

    let source = PathBuf::from(source_path);
    let meta =
        fs::metadata(&source).map_err(|e| format!("Private key file cannot be read: {e}"))?;
    if !meta.is_file() {
        return Err("Private key selection must be a file".to_string());
    }

    let dir = server_key_dir(app, server_id)?;
    let file_name = format!("{}-{}", Uuid::new_v4(), key_filename_from_source(&source));
    let dest = dir.join(file_name);
    fs::copy(&source, &dest).map_err(|e| format!("Failed to import private key file: {e}"))?;
    set_private_permissions(&dest, 0o600);
    Ok(dest.to_string_lossy().into_owned())
}

fn server_key_dir_path(app: &AppHandle, server_id: &str) -> Option<PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|dir| dir.join(SERVER_KEYS_DIR).join(server_id))
}

fn remove_imported_private_keys(app: &AppHandle, server_id: &str) {
    if let Some(dir) = server_key_dir_path(app, server_id) {
        let _ = fs::remove_dir_all(dir);
    }
}

fn prune_imported_private_keys(app: &AppHandle, server_id: &str, keep_path: &str) {
    let Some(dir) = server_key_dir_path(app, server_id) else {
        return;
    };
    let keep = PathBuf::from(keep_path);
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path != keep {
            let _ = fs::remove_file(path);
        }
    }
}

// ---- Row mapping -----------------------------------------------------------

const SELECT_COLS: &str = "id, label, host, port, username, auth_method, private_key_path, \
     tags, status, last_checked_at, created_at";
const PROJECT_SERVER_SELECT_COLS: &str = "servers.id, servers.label, servers.host, servers.port, \
     servers.username, servers.auth_method, servers.private_key_path, servers.tags, servers.status, \
     servers.last_checked_at, servers.created_at";

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

/// Public accessor for other command modules. Returns the server summary —
/// public metadata only, never the passphrase VALUE (SEC-101/SEC-201).
///
/// Kept as a foundation accessor after the GĐ3 deploy module (its sole caller)
/// was removed in the ssh-transparent-connect pivot.
#[allow(dead_code)]
pub(crate) async fn fetch_server_public(db: &Db, id: &str) -> Result<VpsServer, String> {
    fetch_server(db, id).await
}

// ---- CRUD commands ---------------------------------------------------------

#[tauri::command]
pub async fn list_vps_servers(db: State<'_, Db>) -> Result<Vec<VpsServer>, String> {
    let rows = sqlx::query(&format!(
        "SELECT {} FROM servers ORDER BY label",
        SELECT_COLS
    ))
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows.iter().map(row_to_server).collect())
}

#[tauri::command]
pub async fn create_vps_server(
    app: AppHandle,
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
        &payload.private_key_source_path,
    )?;

    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let imported_private_key_path = if fields.auth_method == "key" {
        if let Some(source_path) = trimmed_optional(&payload.private_key_source_path) {
            Some(import_private_key_file(&app, &id, &source_path)?)
        } else {
            fields.private_key_path.clone()
        }
    } else {
        None
    };

    let insert_result = sqlx::query(
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
    .bind(&imported_private_key_path)
    .bind(&payload.tags)
    .bind(&now)
    .execute(db.inner())
    .await;
    if let Err(e) = insert_result {
        if imported_private_key_path.is_some() {
            remove_imported_private_keys(&app, &id);
        }
        return Err(e.to_string());
    }

    if let Some(path) = imported_private_key_path.as_deref() {
        prune_imported_private_keys(&app, &id, path);
    }

    // Store the passphrase only when a non-empty one is supplied.
    if let Some(pass) = payload.passphrase.as_ref().filter(|p| !p.is_empty()) {
        secrets::set_server_secret(&id, pass)
            .map_err(|e| format!("Failed to store passphrase: {}", e))?;
    }

    fetch_server(db.inner(), &id).await
}

#[tauri::command]
pub async fn update_vps_server(
    app: AppHandle,
    db: State<'_, Db>,
    payload: UpdateServerPayload,
) -> Result<VpsServer, String> {
    // Ensure the server exists and keep its existing key path as the default
    // when the user edits other fields without selecting a replacement key.
    let existing = sqlx::query("SELECT private_key_path FROM servers WHERE id = ?")
        .bind(&payload.id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| format!("Server not found: {}", e))?;
    let existing_private_key_path: Option<String> = existing.get("private_key_path");
    let effective_private_key_path = trimmed_optional(&payload.private_key_path)
        .or_else(|| trimmed_optional(&existing_private_key_path));

    // Validate BEFORE any DB write (BR-001..003).
    let fields = validate_payload(
        &payload.label,
        &payload.host,
        &payload.username,
        payload.port,
        &payload.auth_method,
        &effective_private_key_path,
        &payload.private_key_source_path,
    )?;
    let imported_private_key_path = if fields.auth_method == "key" {
        if let Some(source_path) = trimmed_optional(&payload.private_key_source_path) {
            Some(import_private_key_file(&app, &payload.id, &source_path)?)
        } else {
            fields.private_key_path.clone()
        }
    } else {
        None
    };

    sqlx::query(
        "UPDATE servers SET label = ?, host = ?, port = ?, username = ?, auth_method = ?, \
         private_key_path = ?, tags = ? WHERE id = ?",
    )
    .bind(payload.label.trim())
    .bind(payload.host.trim())
    .bind(fields.port)
    .bind(payload.username.trim())
    .bind(&fields.auth_method)
    .bind(&imported_private_key_path)
    .bind(&payload.tags)
    .bind(&payload.id)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    if let Some(path) = imported_private_key_path.as_deref() {
        if trimmed_optional(&payload.private_key_source_path).is_some() {
            prune_imported_private_keys(&app, &payload.id, path);
        }
    } else if fields.auth_method == "agent" {
        remove_imported_private_keys(&app, &payload.id);
    }

    // BR-005 / AC-008: a new non-empty passphrase overwrites; None/"" keeps the
    // stored value untouched.
    if let Some(pass) = payload.passphrase.as_ref().filter(|p| !p.is_empty()) {
        secrets::set_server_secret(&payload.id, pass)
            .map_err(|e| format!("Failed to store passphrase: {}", e))?;
    }

    fetch_server(db.inner(), &payload.id).await
}

#[tauri::command]
pub async fn delete_vps_server(
    app: AppHandle,
    db: State<'_, Db>,
    id: String,
) -> Result<(), String> {
    // BR-004: remove the Keychain secret alongside the row.
    let _ = secrets::delete_server_secret(&id);
    remove_imported_private_keys(&app, &id);
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
/// Command: `ssh -o BatchMode=yes -o ConnectTimeout=10 -o
/// StrictHostKeyChecking=accept-new -p <port> [-i <key>] <user>@<host> echo
/// devdy-ok`. `ok` requires exit 0 AND stdout containing `devdy-ok`. The result
/// (`online`/`offline`) plus `last_checked_at` is written back to the row. Never
/// logs the passphrase or raw ssh stdout.
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
        .arg("-o")
        .arg("StrictHostKeyChecking=accept-new")
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
        PROJECT_SERVER_SELECT_COLS
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
    sqlx::query("DELETE FROM project_servers WHERE project_id = ? AND server_id = ? AND role = ?")
        .bind(&project_id)
        .bind(&server_id)
        .bind(&role)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_key_filename_keeps_safe_names() {
        assert_eq!(sanitize_key_filename("id_ed25519"), "id_ed25519");
        assert_eq!(sanitize_key_filename("prod key.pem"), "prod_key.pem");
        assert_eq!(sanitize_key_filename("../id_rsa"), "_id_rsa");
        assert_eq!(sanitize_key_filename("..."), "id_key");
    }

    #[test]
    fn validate_key_auth_accepts_selected_source_file() {
        let fields = validate_payload(
            "Prod",
            "example.com",
            "root",
            Some(22),
            "key",
            &None,
            &Some("/tmp/id_ed25519".to_string()),
        )
        .unwrap();
        assert_eq!(fields.port, 22);
        assert_eq!(fields.auth_method, "key");
        assert_eq!(fields.private_key_path, None);
    }

    #[test]
    fn validate_key_auth_rejects_missing_key_file() {
        let err = validate_payload("Prod", "example.com", "root", Some(22), "key", &None, &None)
            .unwrap_err();
        assert!(err.contains("private key file"));
    }
}
