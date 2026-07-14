//! Central MCP server management.
//!
//! Servers are defined once (like skills/rules), toggled per project, then
//! injected into a run at launch: Claude gets `options.mcpServers`, Codex gets
//! `-c mcp_servers.*` config overrides (stdio only). Secret env/header VALUEs
//! live in the macOS Keychain — SQLite only holds the KEY names.

use crate::db::Db;
use crate::secrets;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tauri::State;
use uuid::Uuid;

// ---- Wire types ------------------------------------------------------------

/// A single secret key/value row (env var or header) as sent from the UI.
/// `value` is `None` on update when the user leaves an existing key untouched
/// (→ keep the stored VALUE); `Some("")` / `Some(v)` overwrites it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretEntry {
    pub key: String,
    #[serde(default)]
    pub value: Option<String>,
}

/// Server summary for list views (no secret VALUEs, no key names).
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct McpServer {
    pub id: String,
    pub name: String,
    pub description: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub env_keys: Vec<String>,
    pub header_keys: Vec<String>,
    pub enabled: bool,
    pub created_at: String,
}

/// Server + per-project enabled flag (for the project detail assignment UI).
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ProjectMcpServer {
    #[serde(flatten)]
    pub server: McpServer,
    pub enabled_for_project: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateMcpServerPayload {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub transport: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub env: Vec<SecretEntry>,
    #[serde(default)]
    pub headers: Vec<SecretEntry>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMcpServerPayload {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub transport: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub env: Vec<SecretEntry>,
    #[serde(default)]
    pub headers: Vec<SecretEntry>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// Result of a connection test.
#[derive(Debug, Serialize)]
pub struct TestConnectionResult {
    pub ok: bool,
    pub message: String,
}

/// Portable export/import shape — INCLUDES secret VALUEs (user action to a
/// file of their choice; the UI warns the file contains secrets).
#[derive(Debug, Serialize, Deserialize)]
pub struct McpServerExport {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub transport: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

// ---- Validation ------------------------------------------------------------

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("MCP server name cannot be empty".to_string());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(
            "MCP server name must only contain letters, numbers, hyphens, and underscores"
                .to_string(),
        );
    }
    Ok(())
}

/// Validate transport + required fields + secret KEY rows (non-empty, unique).
/// Returns the cleaned (trimmed key) secret rows split into env + headers.
fn validate_shape(
    transport: &str,
    command: &Option<String>,
    url: &Option<String>,
    env: &[SecretEntry],
    headers: &[SecretEntry],
) -> Result<(), String> {
    match transport {
        "stdio" => {
            if command.as_ref().map(|c| c.trim().is_empty()).unwrap_or(true) {
                return Err("stdio transport requires a command".to_string());
            }
        }
        "http" | "sse" => {
            if url.as_ref().map(|u| u.trim().is_empty()).unwrap_or(true) {
                return Err(format!("{} transport requires a url", transport));
            }
        }
        other => {
            return Err(format!(
                "transport must be one of stdio, http, sse (got '{}')",
                other
            ));
        }
    }
    validate_secret_keys(env, "env")?;
    validate_secret_keys(headers, "header")?;
    Ok(())
}

fn validate_secret_keys(entries: &[SecretEntry], label: &str) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for e in entries {
        let key = e.key.trim();
        if key.is_empty() {
            return Err(format!("{} key cannot be empty", label));
        }
        if !seen.insert(key.to_string()) {
            return Err(format!("duplicate {} key '{}'", label, key));
        }
    }
    Ok(())
}

// ---- Row mapping -----------------------------------------------------------

fn parse_json_array(raw: Option<String>) -> Vec<String> {
    raw.and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default()
}

fn row_to_server(row: &sqlx::sqlite::SqliteRow) -> McpServer {
    use sqlx::Row;
    McpServer {
        id: row.get("id"),
        name: row.get("name"),
        description: row.get::<Option<String>, _>("description").unwrap_or_default(),
        transport: row.get("transport"),
        command: row.get("command"),
        args: parse_json_array(row.get("args")),
        url: row.get("url"),
        env_keys: parse_json_array(row.get("env_keys")),
        header_keys: parse_json_array(row.get("header_keys")),
        enabled: row.get::<i64, _>("enabled") != 0,
        created_at: row.get("created_at"),
    }
}

const SELECT_COLS: &str =
    "id, name, description, transport, command, args, url, env_keys, header_keys, enabled, created_at";

// ---- Secret helpers --------------------------------------------------------

/// Collect the KEY names (trimmed) from a set of secret rows.
fn keys_of(entries: &[SecretEntry]) -> Vec<String> {
    entries.iter().map(|e| e.key.trim().to_string()).collect()
}

/// Build a KEY→VALUE map from create-time rows (missing VALUE → empty string).
fn map_from_entries(entries: &[SecretEntry]) -> HashMap<String, String> {
    entries
        .iter()
        .map(|e| (e.key.trim().to_string(), e.value.clone().unwrap_or_default()))
        .collect()
}

/// Merge update rows against the previously stored map: for each key keep the
/// new VALUE when supplied, else fall back to the stored VALUE (or empty).
/// Keys no longer present are dropped. Returns the merged map to persist.
fn merge_secrets(entries: &[SecretEntry], prev: &HashMap<String, String>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for e in entries {
        let key = e.key.trim().to_string();
        let value = match &e.value {
            Some(v) => v.clone(),
            None => prev.get(&key).cloned().unwrap_or_default(),
        };
        out.insert(key, value);
    }
    out
}

// ---- CRUD commands ---------------------------------------------------------

#[tauri::command]
pub async fn list_mcp_servers(db: State<'_, Db>) -> Result<Vec<McpServer>, String> {
    let rows = sqlx::query(&format!(
        "SELECT {} FROM mcp_servers ORDER BY name",
        SELECT_COLS
    ))
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows.iter().map(row_to_server).collect())
}

#[tauri::command]
pub async fn get_mcp_server(db: State<'_, Db>, id: String) -> Result<McpServer, String> {
    let row = sqlx::query(&format!(
        "SELECT {} FROM mcp_servers WHERE id = ?",
        SELECT_COLS
    ))
    .bind(&id)
    .fetch_one(db.inner())
    .await
    .map_err(|e| format!("MCP server not found: {}", e))?;
    // Deliberately returns only env_keys/header_keys — never secret VALUEs.
    Ok(row_to_server(&row))
}

#[tauri::command]
pub async fn create_mcp_server(
    db: State<'_, Db>,
    payload: CreateMcpServerPayload,
) -> Result<McpServer, String> {
    validate_name(&payload.name)?;
    validate_shape(
        &payload.transport,
        &payload.command,
        &payload.url,
        &payload.env,
        &payload.headers,
    )?;

    let existing = sqlx::query("SELECT id FROM mcp_servers WHERE name = ?")
        .bind(&payload.name)
        .fetch_optional(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    if existing.is_some() {
        return Err(format!("MCP server '{}' already exists", payload.name));
    }

    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let env_keys = keys_of(&payload.env);
    let header_keys = keys_of(&payload.headers);
    let (command, url) = normalized_fields(&payload.transport, payload.command, payload.url);

    insert_server_row(
        db.inner(),
        &id,
        &payload.name,
        &payload.description,
        &payload.transport,
        &command,
        &payload.args,
        &url,
        &env_keys,
        &header_keys,
        payload.enabled,
        &now,
    )
    .await?;

    // Persist secret VALUEs to the Keychain.
    secrets::set_mcp_secrets(&id, &map_from_entries(&payload.env), &map_from_entries(&payload.headers))
        .map_err(|e| format!("Failed to store secrets: {}", e))?;

    get_mcp_server(db, id).await
}

#[tauri::command]
pub async fn update_mcp_server(
    db: State<'_, Db>,
    payload: UpdateMcpServerPayload,
) -> Result<McpServer, String> {
    validate_name(&payload.name)?;
    validate_shape(
        &payload.transport,
        &payload.command,
        &payload.url,
        &payload.env,
        &payload.headers,
    )?;

    // Ensure the server exists.
    let _ = sqlx::query("SELECT id FROM mcp_servers WHERE id = ?")
        .bind(&payload.id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| format!("MCP server not found: {}", e))?;

    // Name-uniqueness against OTHER rows.
    let clash = sqlx::query("SELECT id FROM mcp_servers WHERE name = ? AND id <> ?")
        .bind(&payload.name)
        .bind(&payload.id)
        .fetch_optional(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    if clash.is_some() {
        return Err(format!("MCP server '{}' already exists", payload.name));
    }

    let env_keys = keys_of(&payload.env);
    let header_keys = keys_of(&payload.headers);
    let (command, url) = normalized_fields(&payload.transport, payload.command, payload.url);
    let args_json = serde_json::to_string(&payload.args).unwrap_or_else(|_| "[]".to_string());
    let env_keys_json = serde_json::to_string(&env_keys).unwrap_or_else(|_| "[]".to_string());
    let header_keys_json = serde_json::to_string(&header_keys).unwrap_or_else(|_| "[]".to_string());

    sqlx::query(
        "UPDATE mcp_servers SET name = ?, description = ?, transport = ?, command = ?, args = ?, \
         url = ?, env_keys = ?, header_keys = ?, enabled = ? WHERE id = ?",
    )
    .bind(&payload.name)
    .bind(&payload.description)
    .bind(&payload.transport)
    .bind(&command)
    .bind(&args_json)
    .bind(&url)
    .bind(&env_keys_json)
    .bind(&header_keys_json)
    .bind(payload.enabled as i64)
    .bind(&payload.id)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    // Merge secrets: keep stored VALUE for keys with no new value supplied.
    let prev = secrets::get_mcp_secrets(&payload.id);
    let env_map = merge_secrets(&payload.env, &prev.env);
    let header_map = merge_secrets(&payload.headers, &prev.headers);
    secrets::set_mcp_secrets(&payload.id, &env_map, &header_map)
        .map_err(|e| format!("Failed to store secrets: {}", e))?;

    get_mcp_server(db, payload.id).await
}

#[tauri::command]
pub async fn delete_mcp_server(db: State<'_, Db>, id: String) -> Result<(), String> {
    // Rows in project_mcp_servers are removed by the FK ON DELETE CASCADE.
    sqlx::query("DELETE FROM mcp_servers WHERE id = ?")
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    // Remove the Keychain item too.
    let _ = secrets::delete_mcp_secrets(&id);
    Ok(())
}

/// Trim + null-out fields irrelevant to the chosen transport, so the DB stays clean.
fn normalized_fields(
    transport: &str,
    command: Option<String>,
    url: Option<String>,
) -> (Option<String>, Option<String>) {
    let trim = |o: Option<String>| o.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    match transport {
        "stdio" => (trim(command), None),
        _ => (None, trim(url)),
    }
}

#[allow(clippy::too_many_arguments)]
async fn insert_server_row(
    db: &Db,
    id: &str,
    name: &str,
    description: &str,
    transport: &str,
    command: &Option<String>,
    args: &[String],
    url: &Option<String>,
    env_keys: &[String],
    header_keys: &[String],
    enabled: bool,
    created_at: &str,
) -> Result<(), String> {
    let args_json = serde_json::to_string(args).unwrap_or_else(|_| "[]".to_string());
    let env_keys_json = serde_json::to_string(env_keys).unwrap_or_else(|_| "[]".to_string());
    let header_keys_json = serde_json::to_string(header_keys).unwrap_or_else(|_| "[]".to_string());
    sqlx::query(
        "INSERT INTO mcp_servers (id, name, description, transport, command, args, url, \
         env_keys, header_keys, enabled, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(name)
    .bind(description)
    .bind(transport)
    .bind(command)
    .bind(&args_json)
    .bind(url)
    .bind(&env_keys_json)
    .bind(&header_keys_json)
    .bind(enabled as i64)
    .bind(created_at)
    .execute(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

// ---- Per-project assignment ------------------------------------------------

#[tauri::command]
pub async fn list_project_mcp_servers(
    db: State<'_, Db>,
    project_id: String,
) -> Result<Vec<ProjectMcpServer>, String> {
    use sqlx::Row;
    let rows = sqlx::query(&format!(
        "SELECT {}, (SELECT 1 FROM project_mcp_servers pms WHERE pms.server_id = mcp_servers.id \
         AND pms.project_id = ?) AS enabled_for_project FROM mcp_servers ORDER BY name",
        SELECT_COLS
    ))
    .bind(&project_id)
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows
        .iter()
        .map(|row| ProjectMcpServer {
            server: row_to_server(row),
            enabled_for_project: row
                .get::<Option<i64>, _>("enabled_for_project")
                .unwrap_or(0)
                != 0,
        })
        .collect())
}

#[tauri::command]
pub async fn set_project_mcp_servers(
    db: State<'_, Db>,
    project_id: String,
    server_ids: Vec<String>,
) -> Result<(), String> {
    let mut tx = db.inner().begin().await.map_err(|e| e.to_string())?;
    sqlx::query("DELETE FROM project_mcp_servers WHERE project_id = ?")
        .bind(&project_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    let now = chrono::Utc::now().to_rfc3339();
    for sid in &server_ids {
        sqlx::query(
            "INSERT INTO project_mcp_servers (project_id, server_id, enabled_at) VALUES (?, ?, ?)",
        )
        .bind(&project_id)
        .bind(sid)
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    }
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(())
}

// ---- Import / export -------------------------------------------------------

#[tauri::command]
pub async fn export_mcp_server(
    db: State<'_, Db>,
    id: String,
    path: String,
) -> Result<(), String> {
    let server = get_mcp_server(db, id.clone()).await?;
    // Export INCLUDES secret VALUEs (user-initiated to a chosen file).
    let secret = secrets::get_mcp_secrets(&id);
    let export = McpServerExport {
        name: server.name,
        description: server.description,
        transport: server.transport,
        command: server.command,
        args: server.args,
        url: server.url,
        env: secret.env,
        headers: secret.headers,
        enabled: server.enabled,
    };
    let json = serde_json::to_string_pretty(&export).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn import_mcp_server(db: State<'_, Db>, path: String) -> Result<McpServer, String> {
    let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let export: McpServerExport =
        serde_json::from_str(&raw).map_err(|e| format!("Invalid MCP export file: {}", e))?;

    validate_name(&export.name)?;
    // Re-validate shape via SecretEntry projection.
    let env_entries: Vec<SecretEntry> = export
        .env
        .iter()
        .map(|(k, v)| SecretEntry {
            key: k.clone(),
            value: Some(v.clone()),
        })
        .collect();
    let header_entries: Vec<SecretEntry> = export
        .headers
        .iter()
        .map(|(k, v)| SecretEntry {
            key: k.clone(),
            value: Some(v.clone()),
        })
        .collect();
    validate_shape(
        &export.transport,
        &export.command,
        &export.url,
        &env_entries,
        &header_entries,
    )?;

    // Unique name: append a suffix if taken, so import never clobbers.
    let name = unique_name(db.inner(), &export.name).await?;

    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let env_keys: Vec<String> = export.env.keys().cloned().collect();
    let header_keys: Vec<String> = export.headers.keys().cloned().collect();
    let (command, url) = normalized_fields(&export.transport, export.command, export.url);

    insert_server_row(
        db.inner(),
        &id,
        &name,
        &export.description,
        &export.transport,
        &command,
        &export.args,
        &url,
        &env_keys,
        &header_keys,
        export.enabled,
        &now,
    )
    .await?;

    secrets::set_mcp_secrets(&id, &export.env, &export.headers)
        .map_err(|e| format!("Failed to store secrets: {}", e))?;

    get_mcp_server(db, id).await
}

async fn unique_name(db: &Db, base: &str) -> Result<String, String> {
    let mut candidate = base.to_string();
    let mut n = 1;
    loop {
        let hit = sqlx::query("SELECT id FROM mcp_servers WHERE name = ?")
            .bind(&candidate)
            .fetch_optional(db)
            .await
            .map_err(|e| e.to_string())?;
        if hit.is_none() {
            return Ok(candidate);
        }
        n += 1;
        candidate = format!("{}-{}", base, n);
    }
}

// ---- Test connection -------------------------------------------------------

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(9);

fn initialize_request() -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "devdy", "version": "1.0.0" }
        }
    })
}

#[tauri::command]
pub async fn test_mcp_connection(
    db: State<'_, Db>,
    id: String,
) -> Result<TestConnectionResult, String> {
    let server = get_mcp_server(db, id.clone()).await?;
    let secret = secrets::get_mcp_secrets(&id);

    let result = match server.transport.as_str() {
        "stdio" => {
            test_stdio(
                server.command.as_deref().unwrap_or(""),
                &server.args,
                &secret.env,
            )
            .await
        }
        "http" | "sse" => test_http(server.url.as_deref().unwrap_or(""), &secret.headers).await,
        other => TestConnectionResult {
            ok: false,
            message: format!("Unknown transport '{}'", other),
        },
    };
    Ok(result)
}

/// stdio: spawn command+args+env, send an `initialize` JSON-RPC over stdin,
/// read one JSON-RPC response line from stdout. Kill the child afterwards.
async fn test_stdio(
    command: &str,
    args: &[String],
    env: &HashMap<String, String>,
) -> TestConnectionResult {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::process::Command;

    if command.trim().is_empty() {
        return TestConnectionResult {
            ok: false,
            message: "No command configured".to_string(),
        };
    }

    let mut cmd = Command::new(command);
    cmd.args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return TestConnectionResult {
                ok: false,
                message: format!("Failed to spawn '{}': {}", command, e),
            }
        }
    };

    let mut stdin = child.stdin.take();
    let stdout = child.stdout.take();

    let work = async {
        if let Some(stdin) = stdin.as_mut() {
            let line = format!("{}\n", initialize_request());
            stdin.write_all(line.as_bytes()).await.ok();
            stdin.flush().await.ok();
        }
        let stdout = stdout.ok_or_else(|| "No stdout from process".to_string())?;
        let mut reader = BufReader::new(stdout).lines();
        // Read lines until we find a JSON-RPC response with id==1 (skip logs).
        while let Ok(Some(line)) = reader.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if v.get("id").is_some() && v.get("result").is_some() {
                    let name = v
                        .pointer("/result/serverInfo/name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("MCP server");
                    return Ok(format!("Connected to {}", name));
                }
                if let Some(err) = v.get("error") {
                    return Err(format!("Server returned error: {}", err));
                }
            }
        }
        Err("No initialize response received".to_string())
    };

    let outcome = match tokio::time::timeout(HANDSHAKE_TIMEOUT, work).await {
        Ok(Ok(msg)) => TestConnectionResult {
            ok: true,
            message: msg,
        },
        Ok(Err(msg)) => TestConnectionResult {
            ok: false,
            message: msg,
        },
        Err(_) => TestConnectionResult {
            ok: false,
            message: format!("Timed out after {}s", HANDSHAKE_TIMEOUT.as_secs()),
        },
    };

    // Kill regardless of outcome.
    let _ = child.start_kill();
    let _ = child.wait().await;
    outcome
}

/// http/sse: POST an `initialize` JSON-RPC to the URL with headers. Accepts
/// either a JSON body or an SSE `data:` event carrying the JSON-RPC response.
async fn test_http(url: &str, headers: &HashMap<String, String>) -> TestConnectionResult {
    if url.trim().is_empty() {
        return TestConnectionResult {
            ok: false,
            message: "No url configured".to_string(),
        };
    }

    let client = match reqwest::Client::builder().timeout(HANDSHAKE_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            return TestConnectionResult {
                ok: false,
                message: format!("HTTP client error: {}", e),
            }
        }
    };

    let mut req = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&initialize_request());
    for (k, v) in headers {
        req = req.header(k, v);
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            return TestConnectionResult {
                ok: false,
                message: format!("Request failed: {}", e),
            }
        }
    };

    let status = resp.status();
    if !status.is_success() {
        return TestConnectionResult {
            ok: false,
            message: format!("Server responded with HTTP {}", status.as_u16()),
        };
    }

    let body = match resp.text().await {
        Ok(b) => b,
        Err(e) => {
            return TestConnectionResult {
                ok: false,
                message: format!("Failed to read response: {}", e),
            }
        }
    };

    // Try to locate a JSON-RPC result, either as a plain JSON body or inside an
    // SSE `data:` line.
    if let Some(msg) = extract_initialize_result(&body) {
        return TestConnectionResult { ok: true, message: msg };
    }
    // Connected (HTTP 2xx) but no recognizable initialize result — still a
    // meaningful signal that the endpoint is reachable.
    TestConnectionResult {
        ok: true,
        message: format!("Endpoint reachable (HTTP {})", status.as_u16()),
    }
}

fn extract_initialize_result(body: &str) -> Option<String> {
    let try_value = |v: &serde_json::Value| -> Option<String> {
        if v.get("result").is_some() {
            let name = v
                .pointer("/result/serverInfo/name")
                .and_then(|n| n.as_str())
                .unwrap_or("MCP server");
            Some(format!("Connected to {}", name))
        } else {
            None
        }
    };
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body.trim()) {
        if let Some(m) = try_value(&v) {
            return Some(m);
        }
    }
    for line in body.lines() {
        let line = line.trim();
        if let Some(data) = line.strip_prefix("data:") {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(data.trim()) {
                if let Some(m) = try_value(&v) {
                    return Some(m);
                }
            }
        }
    }
    None
}

// ---- Resolve for a run -----------------------------------------------------

/// Build the MCP config to inject for a run on `engine` ("claude" | "codex").
///
/// Returns `(value, skipped)`:
/// - `value` is a `Record<name, config>` JSON object, or `Value::Null` when no
///   server applies.
/// - `skipped` lists server names dropped for the engine (Codex + http/sse).
///
/// Only servers with `enabled = 1` that are assigned to the project count. VALUE
/// secrets are read from the Keychain here — the one place they enter a config.
pub async fn resolve_project_mcp_servers(
    db: &Db,
    project_id: &str,
    engine: &str,
) -> (serde_json::Value, Vec<String>) {
    let rows = match sqlx::query(&format!(
        "SELECT {} FROM mcp_servers WHERE enabled = 1 AND id IN \
         (SELECT server_id FROM project_mcp_servers WHERE project_id = ?) ORDER BY name",
        SELECT_COLS
    ))
    .bind(project_id)
    .fetch_all(db)
    .await
    {
        Ok(r) => r,
        Err(_) => return (serde_json::Value::Null, Vec::new()),
    };

    let mut map = serde_json::Map::new();
    let mut skipped = Vec::new();

    for row in &rows {
        let server = row_to_server(row);
        let secret = secrets::get_mcp_secrets(&server.id);

        match server.transport.as_str() {
            "stdio" => {
                let mut cfg = serde_json::json!({
                    "type": "stdio",
                    "command": server.command.clone().unwrap_or_default(),
                });
                if !server.args.is_empty() {
                    cfg["args"] = serde_json::json!(server.args);
                }
                if !secret.env.is_empty() {
                    cfg["env"] = serde_json::json!(secret.env);
                }
                map.insert(server.name.clone(), cfg);
            }
            "http" | "sse" => {
                if engine == "codex" {
                    // QĐ-1: Codex only supports stdio → drop remote servers.
                    skipped.push(server.name.clone());
                    continue;
                }
                let mut cfg = serde_json::json!({
                    "type": server.transport,
                    "url": server.url.clone().unwrap_or_default(),
                });
                if !secret.headers.is_empty() {
                    cfg["headers"] = serde_json::json!(secret.headers);
                }
                map.insert(server.name.clone(), cfg);
            }
            _ => {}
        }
    }

    if map.is_empty() {
        (serde_json::Value::Null, skipped)
    } else {
        (serde_json::Value::Object(map), skipped)
    }
}
