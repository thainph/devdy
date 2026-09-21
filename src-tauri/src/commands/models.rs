//! Model catalog support — the two engines need opposite treatment.
//!
//! **Codex** publishes a real catalog through `codex debug models`, so its list
//! is DISCOVERED and cached. A failed refresh never drops a good cache: the
//! error lands on the account's entry while the last models and `refreshed_at`
//! survive, and screens fall back to the curated table when a cache is empty.
//!
//! **Claude** has no usable catalog endpoint. The Agent SDK's
//! `supportedModels()` returns only the few rows the CLI picker advertises,
//! which is a strict subset of the ids `--model` accepts — an older pinned
//! version works fine yet never appears. So the Claude list is CURATED in the
//! frontend (`src/lib/engineOptions.ts`) and this module instead offers
//! VALIDATION: `validate_claude_models` runs a one-word turn per id and records
//! which ones the account still serves.
//!
//! Neither path runs automatically — both are user-initiated from Settings, and
//! the normalized result is persisted into the `settings` table keyed by account
//! so every screen can read it back without spawning anything.

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
const CODEX_CACHE_KEY: &str = "codex_models_cache_by_account";
const CODEX_ACTIVE_KEY: &str = "codex_models_active_account_key";

/// The typed cache blob returned to the frontend so it can hydrate its store
/// without triggering any discovery.
#[derive(Debug, Serialize)]
pub struct ModelCaches {
    pub codex: HashMap<String, ModelCacheEntry>,
    pub codex_active_key: String,
    /// Last validation sweep per account — which pinned Claude ids still work.
    pub claude_validation: HashMap<String, ModelValidationReport>,
    pub claude_validation_active_key: String,
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

// ── Claude validation (`claude -p` probe per model) ───────────────────────────

/// One model's verdict from the last validation sweep.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ModelValidation {
    pub model: String,
    /// True when a real turn through the sidecar completed AND the model id was
    /// honored (not silently swapped for a fallback).
    pub ok: bool,
    /// Why it failed, trimmed to something a tooltip can hold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The persisted result of validating the pinned Claude model table against one
/// account. Unlike the Codex cache this is not a catalog — the catalog is
/// curated in the frontend — it only records which entries still work.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ModelValidationReport {
    pub account_key: String,
    pub account_label: String,
    /// ISO timestamp of the sweep that produced `results`.
    pub checked_at: String,
    pub results: Vec<ModelValidation>,
}

/// Settings key holding the validation reports, keyed by account.
const CLAUDE_VALIDATION_KEY: &str = "claude_model_validation_by_account";
const CLAUDE_VALIDATION_ACTIVE_KEY: &str = "claude_model_validation_active_key";

/// How many probes run at once. Each check is a real (tiny) billed turn, so keep
/// the burst small — both to stay gentle on the rate limiter and because each
/// probe spawns its own node sidecar.
const VALIDATE_CONCURRENCY: usize = 3;

/// Seconds a single probe may take before it is treated as a failure.
const VALIDATE_TIMEOUT_SECS: u64 = 120;

/// Everything a probe needs to spawn a sidecar, resolved once per sweep.
#[derive(Clone)]
struct ProbeEnv {
    node_bin: String,
    sidecar_script: std::path::PathBuf,
    claude_path: String,
    config_dir: Option<String>,
}

/// Probe one model id by running the shortest possible real turn THROUGH THE
/// SIDECAR — the same Agent SDK path a run uses, so the binary the SDK resolves
/// (its bundled CLI, or `DEVDY_CLAUDE_PATH` when the user set a custom one) is
/// exactly the one a real run would use. A direct `claude --model` spawn can hit
/// a different binary and give a false verdict; it also can't see that the SDK
/// silently falls back to a default when the id is unrecognized. The sidecar
/// resolves both cases and replies `_devdy_valid` / `_devdy_invalid`.
async fn probe_claude_model(env: ProbeEnv, model: String) -> ModelValidation {
    let cwd = std::env::temp_dir();
    let mut cmd = tokio::process::Command::new(&env.node_bin);
    cmd.current_dir(&cwd).arg(&env.sidecar_script);
    augment_command_path(&mut cmd);
    // Match a real run: only pin the binary when a custom path is configured,
    // otherwise let the SDK use its bundled CLI (see commands/runs.rs).
    if env.claude_path != "claude" && !env.claude_path.trim().is_empty() {
        cmd.env("DEVDY_CLAUDE_PATH", &env.claude_path);
    }
    apply_claude_config_dir(&mut cmd, env.config_dir.as_deref());
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    detach_process_group(&mut cmd);
    cmd.kill_on_drop(true);

    let fail = |model: String, msg: String| ModelValidation {
        model,
        ok: false,
        error: Some(msg),
    };

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return fail(model, format!("Failed to spawn sidecar: {e}")),
    };
    let Some(stdout) = child.stdout.take() else {
        return fail(model, "sidecar produced no stdout".to_string());
    };
    let Some(mut stdin) = child.stdin.take() else {
        return fail(model, "sidecar produced no stdin".to_string());
    };

    let req = serde_json::json!({
        "type": "validate_model",
        "options": { "model": model, "cwd": cwd.to_string_lossy() },
    });
    if let Err(e) = stdin.write_all(format!("{req}\n").as_bytes()).await {
        return fail(model, format!("Failed to send request: {e}"));
    }
    stdin.flush().await.ok();

    let drain = async {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim();
            if !line.starts_with('{') {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            match v.get("type").and_then(|x| x.as_str()) {
                Some("_devdy_valid") => return ModelValidation { model: model.clone(), ok: true, error: None },
                Some("_devdy_invalid") | Some("_devdy_error") => {
                    let msg = v
                        .get("error")
                        .and_then(|e| e.as_str())
                        .map(|s| s.chars().take(300).collect())
                        .unwrap_or_else(|| "Validation failed".to_string());
                    return fail(model.clone(), msg);
                }
                _ => {}
            }
        }
        fail(model.clone(), "Sidecar closed without a verdict".to_string())
    };

    let verdict = match tokio::time::timeout(Duration::from_secs(VALIDATE_TIMEOUT_SECS), drain).await
    {
        Ok(v) => v,
        Err(_) => fail(model.clone(), format!("Timed out after {VALIDATE_TIMEOUT_SECS}s")),
    };
    let _ = child.start_kill();
    let _ = child.wait().await;
    drop(stdin);
    verdict
}

/// Validate the given Claude model ids against the default account by running a
/// one-word turn on each through the sidecar, then persist the report. Replaces
/// the old `supportedModels()` refresh: that call only ever returned the handful
/// of rows the CLI picker advertises, which is a strict subset of the ids
/// `--model` accepts — so it could not answer "is THIS pinned id still served?".
#[tauri::command]
pub async fn validate_claude_models(
    app: AppHandle,
    db: State<'_, Db>,
    models: Vec<String>,
) -> Result<ModelValidationReport, String> {
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
    let config_dir = account.as_ref().map(|a| a.config_dir.clone());

    // Sidecar spawn settings, resolved once and shared by every probe.
    let rows = sqlx::query("SELECT key, value FROM settings")
        .fetch_all(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    let mut node_path = "node".to_string();
    let mut sidecar_path = String::new();
    let mut claude_path = "claude".to_string();
    for row in &rows {
        let key: String = row.get("key");
        let value: String = row.get("value");
        match key.as_str() {
            "node_path" => node_path = value,
            "sidecar_path" => sidecar_path = value,
            "claude_path" if !value.trim().is_empty() => claude_path = value,
            _ => {}
        }
    }
    let (node_bin, sidecar_script) = resolve_sidecar(&app, &node_path, &sidecar_path)?;
    let env = ProbeEnv {
        node_bin,
        sidecar_script,
        claude_path,
        config_dir,
    };

    // Dedupe but keep the caller's order so the UI can zip the report back onto
    // its own table without a lookup.
    let mut wanted: Vec<String> = Vec::new();
    for m in models {
        let m = m.trim().to_string();
        if !m.is_empty() && !wanted.contains(&m) {
            wanted.push(m);
        }
    }

    let mut results: Vec<ModelValidation> = Vec::with_capacity(wanted.len());
    for chunk in wanted.chunks(VALIDATE_CONCURRENCY) {
        let mut set = tokio::task::JoinSet::new();
        for (idx, model) in chunk.iter().enumerate() {
            let (env, model) = (env.clone(), model.clone());
            set.spawn(async move { (idx, probe_claude_model(env, model).await) });
        }
        // JoinSet completes out of order; restore the chunk's order before
        // appending so `results` still lines up with `wanted`.
        let mut done: Vec<(usize, ModelValidation)> = Vec::with_capacity(chunk.len());
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok(pair) => done.push(pair),
                Err(e) => return Err(format!("Validation task failed: {e}")),
            }
        }
        done.sort_by_key(|(idx, _)| *idx);
        results.extend(done.into_iter().map(|(_, v)| v));
    }

    let report = ModelValidationReport {
        account_key: account_key.clone(),
        account_label,
        checked_at: now_iso(),
        results,
    };

    let mut map = parse_validation_map(
        read_setting(db.inner(), CLAUDE_VALIDATION_KEY)
            .await
            .as_deref(),
    );
    map.insert(account_key.clone(), report.clone());
    let serialized = serde_json::to_string(&Map::from_iter(
        map.into_iter()
            .map(|(k, v)| (k, serde_json::to_value(v).unwrap_or(Value::Null))),
    ))
    .map_err(|e| e.to_string())?;
    write_setting(db.inner(), CLAUDE_VALIDATION_KEY, &serialized).await?;
    write_setting(db.inner(), CLAUDE_VALIDATION_ACTIVE_KEY, &account_key).await?;

    Ok(report)
}

/// Parse the stored validation blob into a key → report map. Malformed or
/// missing values decode to an empty map rather than erroring.
fn parse_validation_map(raw: Option<&str>) -> HashMap<String, ModelValidationReport> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return HashMap::new();
    };
    serde_json::from_str::<HashMap<String, ModelValidationReport>>(raw).unwrap_or_default()
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

/// Pick the cache entry a selector should use: the active account's, else the
/// global `default` one, else whatever exists. Mirrors the frontend `pickEntry`
/// so every surface resolves the same list.
fn pick_entry<'a>(
    map: &'a HashMap<String, ModelCacheEntry>,
    active_key: &str,
) -> Option<&'a ModelCacheEntry> {
    map.get(active_key)
        .or_else(|| map.get("default"))
        .or_else(|| map.values().next())
}

/// The discovered Codex model list for the currently active account, with no
/// discovery triggered.
///
/// Exists so non-Tauri callers — the Remote Control forwarder — can ship the
/// same Codex models the desktop composer shows. An empty vector means nothing
/// has been discovered yet, and the caller should fall back to the curated
/// table. Claude has no counterpart: its list is curated in the frontend, so
/// both surfaces already read it from their own bundle.
pub async fn active_discovered_models(db: &Db) -> Vec<ModelOption> {
    let codex_map = parse_cache_map(read_setting(db, CODEX_CACHE_KEY).await.as_deref());
    let codex_key = read_setting(db, CODEX_ACTIVE_KEY)
        .await
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "default".to_string());
    pick_entry(&codex_map, &codex_key)
        .map(|e| e.models.clone())
        .unwrap_or_default()
}

/// Read the persisted Codex cache and Claude validation report (neither triggers
/// any process). Screens call this on load so the dropdowns can render the last
/// known state immediately.
#[tauri::command]
pub async fn get_model_caches(db: State<'_, Db>) -> Result<ModelCaches, String> {
    let db = db.inner();
    Ok(ModelCaches {
        codex: parse_cache_map(read_setting(db, CODEX_CACHE_KEY).await.as_deref()),
        codex_active_key: read_setting(db, CODEX_ACTIVE_KEY)
            .await
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "default".to_string()),
        claude_validation: parse_validation_map(
            read_setting(db, CLAUDE_VALIDATION_KEY).await.as_deref(),
        ),
        claude_validation_active_key: read_setting(db, CLAUDE_VALIDATION_ACTIVE_KEY)
            .await
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "default".to_string()),
    })
}

