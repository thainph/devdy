//! Model discovery — fetch the models the account can actually use.
//!
//! Claude exposes this via the Agent SDK's `supportedModels()` (surfaced by the
//! sidecar's `list_models` command). Codex exposes a raw catalog through the
//! `codex debug models` CLI command. Neither list is fetched automatically:
//! the app only refreshes when the user clicks Refresh in Settings, and the
//! normalized result is persisted into the `settings` table keyed by account so
//! every screen can read the cache back without spawning anything.
//!
//! The refresh commands never drop a good cache on failure — a failed refresh
//! records the error on the account's entry while leaving the last models and
//! `refreshed_at` intact. Screens fall back to the curated alias list when a
//! cache is empty.

use crate::db::Db;
use crate::runs::sidecar::{
    apply_claude_config_dir, augment_command_path, detach_process_group, resolve_sidecar,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sqlx::Row;
use std::collections::HashMap;
use std::time::Duration;
use tauri::{AppHandle, State};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// A model choice surfaced to the UI (mirrors the frontend SelectOption).
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ModelOption {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

/// One persisted cache entry, keyed by account/profile. Stored as JSON inside
/// the `settings` table under the `*_models_cache_by_account` keys so the schema
/// is already multi-account ready even though only the `default` key is used
/// today.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ModelCacheEntry {
    /// Account/profile key this cache belongs to ("default" when global).
    pub account_key: String,
    /// Human-friendly label shown so the user knows which account the cache is for.
    pub account_label: String,
    /// Where the list came from, e.g. "supportedModels()" or "codex debug models".
    pub source: String,
    /// ISO timestamp of the last SUCCESSFUL refresh (kept across failed refreshes).
    pub refreshed_at: Option<String>,
    /// Normalized model list from the last successful refresh.
    pub models: Vec<ModelOption>,
    /// Error message from the last FAILED refresh (cleared on success).
    pub error: Option<String>,
    /// ISO timestamp of the last failed refresh.
    pub error_at: Option<String>,
}

/// Settings keys that hold the persisted caches.
const CLAUDE_CACHE_KEY: &str = "claude_models_cache_by_account";
const CLAUDE_ACTIVE_KEY: &str = "claude_models_active_account_key";
const CODEX_CACHE_KEY: &str = "codex_models_cache_by_account";
const CODEX_ACTIVE_KEY: &str = "codex_models_active_account_key";

/// The typed cache blob returned to the frontend so it can hydrate its store
/// without triggering any discovery.
#[derive(Debug, Serialize)]
pub struct ModelCaches {
    pub claude: HashMap<String, ModelCacheEntry>,
    pub codex: HashMap<String, ModelCacheEntry>,
    pub claude_active_key: String,
    pub codex_active_key: String,
}

// ── settings helpers ─────────────────────────────────────────────────────────

async fn read_setting(db: &Db, key: &str) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
}

async fn write_setting(db: &Db, key: &str, value: &str) -> Result<(), String> {
    sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)")
        .bind(key)
        .bind(value)
        .execute(db)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Parse a stored `*_models_cache_by_account` JSON object into a key → entry map.
/// Malformed / missing values decode to an empty map rather than erroring.
fn parse_cache_map(raw: Option<&str>) -> HashMap<String, ModelCacheEntry> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return HashMap::new();
    };
    serde_json::from_str::<HashMap<String, ModelCacheEntry>>(raw).unwrap_or_default()
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ── Claude discovery (sidecar `supportedModels()` bridge) ─────────────────────

/// Discover the Claude models the default account can use, via the sidecar's
/// `supportedModels()` bridge. Returns canonical model ids + display names.
async fn discover_claude_models(app: &AppHandle, db: &Db) -> Result<Vec<ModelOption>, String> {
    // ── settings needed to spawn the Claude sidecar ───────────────────────────
    let rows = sqlx::query("SELECT key, value FROM settings")
        .fetch_all(db)
        .await
        .map_err(|e| e.to_string())?;
    let mut node_path = "node".to_string();
    let mut sidecar_path = String::new();
    let mut claude_path = String::new();
    for row in &rows {
        let key: String = row.get("key");
        let value: String = row.get("value");
        match key.as_str() {
            "node_path" => node_path = value,
            "sidecar_path" => sidecar_path = value,
            "claude_path" => claude_path = value,
            _ => {}
        }
    }

    let (node_bin, sidecar_script) = resolve_sidecar(app, &node_path, &sidecar_path)?;
    let cwd = std::env::temp_dir();
    let mut cmd = tokio::process::Command::new(&node_bin);
    cmd.current_dir(&cwd).arg(&sidecar_script);
    augment_command_path(&mut cmd);
    if claude_path != "claude" && !claude_path.trim().is_empty() {
        cmd.env("DEVDY_CLAUDE_PATH", &claude_path);
    }
    // Discover models against the default Claude account's profile (if any).
    let claude_account = crate::commands::claude_accounts::default_runtime_account(db).await?;
    apply_claude_config_dir(&mut cmd, claude_account.as_ref().map(|a| a.config_dir.as_str()));
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    detach_process_group(&mut cmd);
    cmd.kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn sidecar ({node_bin}): {e}"))?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;

    let msg = serde_json::json!({
        "type": "list_models",
        "options": { "cwd": cwd.to_string_lossy() },
    });
    stdin
        .write_all(format!("{msg}\n").as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    stdin.flush().await.ok();

    // ── drain until the models arrive (or the sidecar errors) ─────────────────
    let mut models: Vec<ModelOption> = Vec::new();
    let mut sidecar_error: Option<String> = None;
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match v.get("type").and_then(|x| x.as_str()) {
            Some("_devdy_models") => {
                if let Some(arr) = v.get("models").and_then(|m| m.as_array()) {
                    for m in arr {
                        let value = m.get("value").and_then(|x| x.as_str()).unwrap_or("");
                        if value.is_empty() {
                            continue;
                        }
                        let label = m
                            .get("displayName")
                            .and_then(|x| x.as_str())
                            .filter(|s| !s.is_empty())
                            .unwrap_or(value)
                            .to_string();
                        let description = m
                            .get("description")
                            .and_then(|x| x.as_str())
                            .filter(|s| !s.is_empty())
                            .map(str::to_string);
                        models.push(ModelOption {
                            value: value.to_string(),
                            label,
                            description,
                        });
                    }
                }
                break;
            }
            Some("_devdy_error") => {
                sidecar_error = v.get("error").and_then(|e| e.as_str()).map(str::to_string);
                break;
            }
            _ => {}
        }
    }

    let _ = child.start_kill();
    let _ = child.wait().await;
    drop(stdin);

    if models.is_empty() {
        if let Some(err) = sidecar_error {
            return Err(err);
        }
    }
    Ok(models)
}

/// List the Claude models the signed-in account can use. Kept as a read-only
/// command for backward compatibility; the UI now prefers `refresh_claude_models`
/// (which persists the result) and `get_model_caches` (which reads it back).
#[tauri::command]
pub async fn list_claude_models(
    app: AppHandle,
    db: State<'_, Db>,
) -> Result<Vec<ModelOption>, String> {
    discover_claude_models(&app, db.inner()).await
}

// ── Codex discovery (`codex debug models` CLI) ────────────────────────────────

/// Run `codex debug models` and normalize the raw catalog into model options.
async fn discover_codex_models(codex_path: &str) -> Result<Vec<ModelOption>, String> {
    let mut cmd = tokio::process::Command::new(codex_path);
    cmd.arg("debug").arg("models");
    augment_command_path(&mut cmd);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    detach_process_group(&mut cmd);
    cmd.kill_on_drop(true);

    let child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn `{codex_path} debug models`: {e}"))?;

    // Short timeout: the catalog is a quick local read. `kill_on_drop` reaps the
    // child if we time out and drop the future.
    let output = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .map_err(|_| "`codex debug models` timed out after 15s".to_string())?
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        let detail = if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        };
        return Err(format!(
            "`codex debug models` exited with {}{detail}",
            output.status
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_codex_models(&stdout)
}

/// Parse the JSON emitted by `codex debug models` into normalized model options.
///
/// Rules (per the refresh plan): take `.models[]`, drop items without `.slug`,
/// keep only `visibility == "list"` (items without a visibility field are kept),
/// sort by ascending `priority`, and map slug/display_name/description.
fn parse_codex_models(stdout: &str) -> Result<Vec<ModelOption>, String> {
    let v = parse_json_loose(stdout)
        .ok_or_else(|| "Could not parse JSON from `codex debug models`".to_string())?;
    let arr = v
        .get("models")
        .and_then(|m| m.as_array())
        .ok_or_else(|| "`codex debug models` output has no `.models` array".to_string())?;

    let mut scored: Vec<(i64, usize, ModelOption)> = Vec::new();
    for (idx, m) in arr.iter().enumerate() {
        let slug = match m.get("slug").and_then(|x| x.as_str()) {
            Some(s) if !s.trim().is_empty() => s.trim(),
            _ => continue,
        };
        // Only surface models the CLI marks as listable; keep entries that omit
        // the field so an unexpected schema doesn't silently hide everything.
        if let Some(vis) = m.get("visibility").and_then(|x| x.as_str()) {
            if vis != "list" {
                continue;
            }
        }
        let label = m
            .get("display_name")
            .and_then(|x| x.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(slug)
            .to_string();
        let description = m
            .get("description")
            .and_then(|x| x.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_string());
        let priority = m.get("priority").and_then(|x| x.as_i64()).unwrap_or(i64::MAX);
        scored.push((
            priority,
            idx,
            ModelOption {
                value: slug.to_string(),
                label,
                description,
            },
        ));
    }

    // Stable sort by priority, then original order as a tiebreak.
    scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    Ok(scored.into_iter().map(|(_, _, m)| m).collect())
}

/// Parse JSON that may be surrounded by non-JSON log noise: try the whole trimmed
/// string first, then the widest `{...}` slice.
fn parse_json_loose(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return Some(v);
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<Value>(&trimmed[start..=end]).ok()
}

// ── refresh + persist ─────────────────────────────────────────────────────────

/// Apply a discovery result onto an existing (or fresh) cache entry, preserving
/// the previous models/`refreshed_at` when the refresh failed or returned empty.
fn merge_refresh(
    prev: Option<ModelCacheEntry>,
    account_key: &str,
    account_label: &str,
    source: &str,
    outcome: Result<Vec<ModelOption>, String>,
) -> ModelCacheEntry {
    let mut entry = prev.unwrap_or(ModelCacheEntry {
        account_key: account_key.to_string(),
        account_label: account_label.to_string(),
        source: source.to_string(),
        refreshed_at: None,
        models: Vec::new(),
        error: None,
        error_at: None,
    });
    entry.account_key = account_key.to_string();
    entry.account_label = account_label.to_string();
    entry.source = source.to_string();

    match outcome {
        Ok(models) if !models.is_empty() => {
            entry.models = models;
            entry.refreshed_at = Some(now_iso());
            entry.error = None;
            entry.error_at = None;
        }
        Ok(_) => {
            entry.error = Some("No models were returned.".to_string());
            entry.error_at = Some(now_iso());
        }
        Err(e) => {
            entry.error = Some(e);
            entry.error_at = Some(now_iso());
        }
    }
    entry
}

async fn persist_entry(
    db: &Db,
    cache_key: &str,
    active_key: &str,
    account_key: &str,
    entry: &ModelCacheEntry,
) -> Result<(), String> {
    let mut map = parse_cache_map(read_setting(db, cache_key).await.as_deref());
    map.insert(account_key.to_string(), entry.clone());
    let serialized = serde_json::to_string(&Map::from_iter(
        map.into_iter()
            .map(|(k, v)| (k, serde_json::to_value(v).unwrap_or(Value::Null))),
    ))
    .map_err(|e| e.to_string())?;
    write_setting(db, cache_key, &serialized).await?;
    write_setting(db, active_key, account_key).await?;
    Ok(())
}

/// Refresh the Claude model list via `supportedModels()` and persist it to the
/// settings cache under the default account's key. Returns the resulting cache
/// entry (which carries `error` when discovery failed but the cache was kept).
#[tauri::command]
pub async fn refresh_claude_models(
    app: AppHandle,
    db: State<'_, Db>,
) -> Result<ModelCacheEntry, String> {
    let account = crate::commands::claude_accounts::default_runtime_account(db.inner()).await?;
    let account_key = account
        .as_ref()
        .map(|a| a.id.clone())
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| "default".to_string());
    let account_label = account
        .as_ref()
        .map(|a| a.label.clone())
        .filter(|l| !l.trim().is_empty())
        .unwrap_or_else(|| "Global Claude profile".to_string());

    let outcome = discover_claude_models(&app, db.inner()).await;

    let prev = parse_cache_map(read_setting(db.inner(), CLAUDE_CACHE_KEY).await.as_deref())
        .remove(&account_key);
    let entry = merge_refresh(
        prev,
        &account_key,
        &account_label,
        "supportedModels()",
        outcome,
    );
    persist_entry(
        db.inner(),
        CLAUDE_CACHE_KEY,
        CLAUDE_ACTIVE_KEY,
        &account_key,
        &entry,
    )
    .await?;
    Ok(entry)
}

/// Refresh the Codex model list via `codex debug models` and persist it under the
/// `default` account key. Codex has no multi-account concept in Devdy yet, so the
/// key is fixed for now but the schema is already account-scoped.
#[tauri::command]
pub async fn refresh_codex_models(
    _app: AppHandle,
    db: State<'_, Db>,
) -> Result<ModelCacheEntry, String> {
    let codex_path = read_setting(db.inner(), "codex_path")
        .await
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "codex".to_string());

    let account_key = "default".to_string();
    let account_label = "Global Codex login".to_string();

    let outcome = discover_codex_models(&codex_path).await;

    let prev = parse_cache_map(read_setting(db.inner(), CODEX_CACHE_KEY).await.as_deref())
        .remove(&account_key);
    let entry = merge_refresh(
        prev,
        &account_key,
        &account_label,
        "codex debug models",
        outcome,
    );
    persist_entry(
        db.inner(),
        CODEX_CACHE_KEY,
        CODEX_ACTIVE_KEY,
        &account_key,
        &entry,
    )
    .await?;
    Ok(entry)
}

/// Read the persisted model caches (no discovery). Screens call this on load so
/// dropdowns show the last refreshed lists without spawning any process.
#[tauri::command]
pub async fn get_model_caches(db: State<'_, Db>) -> Result<ModelCaches, String> {
    let db = db.inner();
    Ok(ModelCaches {
        claude: parse_cache_map(read_setting(db, CLAUDE_CACHE_KEY).await.as_deref()),
        codex: parse_cache_map(read_setting(db, CODEX_CACHE_KEY).await.as_deref()),
        claude_active_key: read_setting(db, CLAUDE_ACTIVE_KEY)
            .await
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "default".to_string()),
        codex_active_key: read_setting(db, CODEX_ACTIVE_KEY)
            .await
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "default".to_string()),
    })
}
