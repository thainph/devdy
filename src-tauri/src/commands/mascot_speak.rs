//! Mascot Speak - one-shot LLM line for the DY Cyber Fox.
//!
//! This mirrors the lightweight translation command: spawn a sidecar for one
//! read-only turn, disable tools/permissions, collect the assistant text, then
//! return a short speech-bubble line. The frontend only calls this at important
//! moments, and only when the opt-in setting is enabled.

use crate::db::Db;
use crate::runs::sidecar::{
    apply_claude_config_dir, augment_command_path, detach_process_group, resolve_codex_sidecar,
    resolve_sidecar,
};
use serde_json::Value;
use sqlx::Row;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, State};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex as TokioMutex;

const MAX_CONTEXT_CHARS: usize = 4_000;
const MAX_OUTPUT_CHARS: usize = 180;

#[derive(Default)]
pub struct MascotSpeakState {
    child: Arc<TokioMutex<Option<tokio::process::Child>>>,
    generation: Arc<AtomicU64>,
}

#[tauri::command]
pub async fn cancel_mascot_speak(state: State<'_, MascotSpeakState>) -> Result<(), String> {
    state.generation.fetch_add(1, Ordering::SeqCst);
    if let Some(mut child) = state.child.lock().await.take() {
        let _ = child.start_kill();
    }
    Ok(())
}

fn language_name(locale: &str) -> &'static str {
    let l = locale.trim().to_lowercase();
    if l.starts_with("vi") {
        "Vietnamese"
    } else {
        "English"
    }
}

fn variant_instruction(variant: &str) -> &'static str {
    match variant.trim().to_lowercase().as_str() {
        "permission" => {
            "The app is waiting for user permission. Mention the requested tool or action if it is clear."
        }
        "error" => {
            "The run failed, was cancelled, or hit an error. Be concise and useful without exaggerating."
        }
        "success" => "The run finished. React to the final result or summary if it is clear.",
        _ => "React briefly to the current app event.",
    }
}

fn clean_line(raw: &str) -> String {
    let mut s = raw
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .replace(['\r', '\n'], " ");
    while s.contains("  ") {
        s = s.replace("  ", " ");
    }
    if s.chars().count() > MAX_OUTPUT_CHARS {
        let mut clipped: String = s.chars().take(MAX_OUTPUT_CHARS.saturating_sub(3)).collect();
        clipped = clipped.trim_end().to_string();
        clipped.push_str("...");
        clipped
    } else {
        s
    }
}

#[tauri::command]
pub async fn mascot_speak(
    app: AppHandle,
    db: State<'_, Db>,
    state: State<'_, MascotSpeakState>,
    context: String,
    variant: String,
    locale: String,
) -> Result<String, String> {
    let trimmed = context.trim();
    if trimmed.is_empty() {
        return Err("No mascot context provided.".to_string());
    }
    let source: String = trimmed.chars().take(MAX_CONTEXT_CHARS).collect();
    let lang = language_name(&locale);
    let instruction = variant_instruction(&variant);

    let rows = sqlx::query("SELECT key, value FROM settings")
        .fetch_all(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    let mut node_path = "node".to_string();
    let mut sidecar_path = String::new();
    let mut codex_sidecar_path = String::new();
    let mut claude_path = String::new();
    let mut codex_path = String::new();
    let mut default_engine = "codex".to_string();
    let mut engine = String::new();
    let mut model = String::new();
    for row in &rows {
        let key: String = row.get("key");
        let value: String = row.get("value");
        match key.as_str() {
            "node_path" => node_path = value,
            "sidecar_path" => sidecar_path = value,
            "codex_sidecar_path" => codex_sidecar_path = value,
            "claude_path" => claude_path = value,
            "codex_path" => codex_path = value,
            "default_engine" if !value.trim().is_empty() => default_engine = value,
            "mascot_speech_engine" if !value.trim().is_empty() => engine = value,
            "mascot_speech_model" => model = value,
            _ => {}
        }
    }
    if engine.trim().is_empty() {
        engine = default_engine;
    }

    let prompt = format!(
        "You are Cyber Fox, Devdy's assistant mascot. Read the app event context delimited by <<<>>> and write exactly ONE short speech-bubble line in {lang}.\n\
         Requirements:\n\
         - Maximum 160 characters.\n\
         - One sentence only.\n\
         - No markdown, no bullet list, no quotes, no preamble.\n\
         - Do not pretend to be the main assistant. Do not invent details absent from the context.\n\
         - Warm, playful, and practical.\n\
         Event type: {variant}\n\
         {instruction}\n\n\
         <<<\n{source}\n>>>",
        lang = lang,
        variant = variant.trim(),
        instruction = instruction,
        source = source,
    );

    let is_codex = engine.trim().eq_ignore_ascii_case("codex");
    let (node_bin, sidecar_script) = if is_codex {
        resolve_codex_sidecar(&app, &node_path, &codex_sidecar_path)?
    } else {
        resolve_sidecar(&app, &node_path, &sidecar_path)?
    };
    let cwd = std::env::temp_dir();
    let mut cmd = tokio::process::Command::new(&node_bin);
    cmd.current_dir(&cwd).arg(&sidecar_script);
    augment_command_path(&mut cmd);
    if is_codex {
        if codex_path != "codex" && !codex_path.trim().is_empty() {
            cmd.env("DEVDY_CODEX_PATH", &codex_path);
        }
        cmd.env("DEVDY_PERMISSION_MODE", "bypassPermissions");
        if !model.trim().is_empty() {
            cmd.env("DEVDY_CODEX_MODEL", model.trim());
        }
    } else {
        if claude_path != "claude" && !claude_path.trim().is_empty() {
            cmd.env("DEVDY_CLAUDE_PATH", &claude_path);
        }
        // Speak using the default Claude account's profile (if any).
        let claude_account =
            crate::commands::claude_accounts::default_runtime_account(db.inner()).await?;
        apply_claude_config_dir(&mut cmd, claude_account.as_ref().map(|a| a.config_dir.as_str()));
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

    let my_gen = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
    if let Some(mut old) = state.child.lock().await.replace(child) {
        let _ = old.start_kill();
        tokio::spawn(async move {
            let _ = old.wait().await;
        });
    }

    if is_codex {
        let msg = serde_json::json!({ "type": "prompt", "text": prompt });
        stdin
            .write_all(format!("{msg}\n").as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stdin.flush().await.ok();
    } else {
        let mut options = serde_json::json!({
            "cwd": cwd.to_string_lossy(),
            "permissionMode": "bypassPermissions",
            "allowedTools": [],
            "includePartialMessages": false,
        });
        let m = if model.trim().is_empty() {
            "haiku".to_string()
        } else {
            model.trim().to_string()
        };
        options["model"] = Value::String(m);
        let msg = serde_json::json!({ "type": "prompt", "text": prompt, "options": options });
        stdin
            .write_all(format!("{msg}\n").as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        stdin.flush().await.ok();
        stdin.write_all(b"{\"type\":\"end_input\"}\n").await.ok();
        stdin.flush().await.ok();
    }

    let mut collected = String::new();
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
            Some("_devdy_error") => {
                sidecar_error = v.get("error").and_then(|e| e.as_str()).map(str::to_string);
            }
            Some("assistant") => {
                if let Some(content) = v.pointer("/message/content").and_then(|c| c.as_array()) {
                    for block in content {
                        if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                            if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                                collected.push_str(t);
                            }
                        }
                    }
                }
            }
            Some("result") | Some("_devdy_done") => break,
            _ => {}
        }
    }

    if state.generation.load(Ordering::SeqCst) == my_gen {
        if let Some(mut current) = state.child.lock().await.take() {
            let _ = current.start_kill();
            let _ = current.wait().await;
        }
    }
    drop(stdin);

    let out = clean_line(&collected);
    if out.is_empty() {
        return Err(sidecar_error.unwrap_or_else(|| "No mascot line generated.".to_string()));
    }
    Ok(out)
}
