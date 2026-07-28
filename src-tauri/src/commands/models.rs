//! Model discovery — fetch the models the account can actually use.
//!
//! Claude exposes this via the Agent SDK's `supportedModels()` (surfaced by the
//! sidecar's `list_models` command), so we can auto-append newly released models
//! to the curated alias list in the UI without a hardcode bump. Auth is the same
//! Keychain/subscription login as a normal run — no API key.
//!
//! Codex has no equivalent: `codex app-server` exposes no model-list RPC and the
//! `codex` CLI has no `model list` subcommand, so Codex stays on a curated list.

use crate::db::Db;
use crate::runs::sidecar::{augment_command_path, detach_process_group, resolve_sidecar};
use serde::Serialize;
use serde_json::Value;
use sqlx::Row;
use tauri::{AppHandle, State};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// A model choice surfaced to the UI (mirrors the frontend SelectOption).
#[derive(Debug, Serialize)]
pub struct ModelOption {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

/// List the Claude models the signed-in account can use, via the sidecar's
/// `supportedModels()` bridge. Returns canonical model ids + display names.
#[tauri::command]
pub async fn list_claude_models(
    app: AppHandle,
    db: State<'_, Db>,
) -> Result<Vec<ModelOption>, String> {
    // ── settings needed to spawn the Claude sidecar ───────────────────────────
    let rows = sqlx::query("SELECT key, value FROM settings")
        .fetch_all(db.inner())
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

    let (node_bin, sidecar_script) = resolve_sidecar(&app, &node_path, &sidecar_path)?;
    let cwd = std::env::temp_dir();
    let mut cmd = tokio::process::Command::new(&node_bin);
    cmd.current_dir(&cwd).arg(&sidecar_script);
    augment_command_path(&mut cmd);
    if claude_path != "claude" && !claude_path.trim().is_empty() {
        cmd.env("DEVDY_CLAUDE_PATH", &claude_path);
    }
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
