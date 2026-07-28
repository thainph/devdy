//! Translate — one-shot LLM translation of a short text selection.
//!
//! The "AI result" view lets the user select any text and translate it. Rather
//! than routing through the full run machinery (a persistent, multi-turn
//! session with permission gating), this command spawns the Agent-SDK sidecar
//! for a SINGLE read-only turn: no tools (`allowedTools: []`), auto-approved
//! (`permissionMode: bypassPermissions`), and returns the assistant text as the
//! command result — no streaming events, the frontend just awaits once.
//!
//! Engine/model/style come from the user's settings (`translate_engine`,
//! `translate_model`, `translate_style`, `translate_target_lang`); the caller
//! may override the target language per call (the popover's quick toggle).
//! Both engines are supported by branching on the sidecar (`resolve_sidecar` for
//! Claude, `resolve_codex_sidecar` for Codex), mirroring how runs pick a sidecar.
//!
//! Auth is inherited exactly like a normal run: the SDK/CLI reads the same
//! Keychain / ChatGPT login, so translations bill against the subscription with
//! no API key.

use crate::db::Db;
use crate::runs::sidecar::{
    augment_command_path, detach_process_group, resolve_codex_sidecar, resolve_sidecar,
};
use serde_json::Value;
use sqlx::Row;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, State};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex as TokioMutex;

/// Cap on how much text we translate in one call — keeps the turn snappy and
/// avoids handing the model a whole transcript by accident.
const MAX_INPUT_CHARS: usize = 8_000;

/// Shared handle to the in-flight translation sidecar so a new request (or an
/// explicit cancel) can tear the previous one down instead of piling up node
/// processes when the user translates several selections quickly.
#[derive(Default)]
pub struct TranslateState {
    child: Arc<TokioMutex<Option<tokio::process::Child>>>,
    /// Bumped on every start / cancel so a superseded request's final reap never
    /// kills a newer request's sidecar.
    generation: Arc<AtomicU64>,
}

/// Cancel the in-flight translation (if any).
#[tauri::command]
pub async fn cancel_translate(state: State<'_, TranslateState>) -> Result<(), String> {
    state.generation.fetch_add(1, Ordering::SeqCst);
    if let Some(mut child) = state.child.lock().await.take() {
        let _ = child.start_kill();
    }
    Ok(())
}

/// Map a language code/name to a human-readable target name for the prompt.
fn target_language_name(code: &str) -> String {
    match code.trim().to_lowercase().as_str() {
        "vi" | "vietnamese" | "tiếng việt" => "Vietnamese".to_string(),
        "en" | "english" => "English".to_string(),
        "ja" | "japanese" | "日本語" => "Japanese".to_string(),
        "zh" | "chinese" => "Chinese (Simplified)".to_string(),
        "ko" | "korean" => "Korean".to_string(),
        "fr" | "french" => "French".to_string(),
        "de" | "german" => "German".to_string(),
        "es" | "spanish" => "Spanish".to_string(),
        // Unknown → hand the raw value to the model as-is.
        other if !other.is_empty() => code.trim().to_string(),
        _ => "Vietnamese".to_string(),
    }
}

/// Per-style instruction appended to the base translation prompt.
fn style_instruction(style: &str) -> &'static str {
    match style.trim().to_lowercase().as_str() {
        "literal" => "Translate as literally and faithfully as possible, staying close to the original sentence structure.",
        "technical" => "Keep technical terms, proper nouns, product names, code, and identifiers unchanged; translate only the surrounding prose. Aim for accuracy over fluency.",
        "formal" => "Use a formal, polite register suitable for professional documents.",
        "casual" => "Use a natural, casual, conversational register.",
        // "natural" and anything unknown.
        _ => "Translate naturally and fluently, conveying the meaning rather than a word-for-word rendering.",
    }
}

#[tauri::command]
pub async fn translate_text(
    app: AppHandle,
    db: State<'_, Db>,
    state: State<'_, TranslateState>,
    text: String,
    target_lang: Option<String>,
) -> Result<String, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("Không có nội dung để dịch.".to_string());
    }
    let source: String = trimmed.chars().take(MAX_INPUT_CHARS).collect();

    // ── settings: engine/model/style/target + sidecar paths ───────────────────
    let rows = sqlx::query("SELECT key, value FROM settings")
        .fetch_all(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    let mut node_path = "node".to_string();
    let mut sidecar_path = String::new();
    let mut codex_sidecar_path = String::new();
    let mut claude_path = String::new();
    let mut codex_path = String::new();
    let mut engine = "claude".to_string();
    let mut model = String::new();
    let mut style = "natural".to_string();
    let mut default_target = "vi".to_string();
    for row in &rows {
        let key: String = row.get("key");
        let value: String = row.get("value");
        match key.as_str() {
            "node_path" => node_path = value,
            "sidecar_path" => sidecar_path = value,
            "codex_sidecar_path" => codex_sidecar_path = value,
            "claude_path" => claude_path = value,
            "codex_path" => codex_path = value,
            "translate_engine" if !value.trim().is_empty() => engine = value,
            "translate_model" => model = value,
            "translate_style" if !value.trim().is_empty() => style = value,
            "translate_target_lang" if !value.trim().is_empty() => default_target = value,
            _ => {}
        }
    }
    let target = target_lang
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or(default_target);
    let target_name = target_language_name(&target);

    let prompt = format!(
        "You are a professional translator. Translate the text delimited by <<<>>> into {target_name}.\n\
         {style}\n\
         Output ONLY the translation — no preamble, no explanation, no quotes, no notes. \
         Preserve markdown formatting, code blocks, and inline code unchanged. If the text is \
         already in {target_name}, return it unchanged.\n\n\
         <<<\n{source}\n>>>",
        target_name = target_name,
        style = style_instruction(&style),
        source = source,
    );

    let is_codex = engine.trim().eq_ignore_ascii_case("codex");

    // ── spawn the sidecar for the chosen engine ───────────────────────────────
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
    } else if claude_path != "claude" && !claude_path.trim().is_empty() {
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

    // Supersede any previous in-flight translation, then remember this one so a
    // newer request / cancel can kill it. The generation marks THIS request as
    // current; a later request bumps it and takes ownership of `state.child`.
    let my_gen = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
    if let Some(mut old) = state.child.lock().await.replace(child) {
        let _ = old.start_kill();
        tokio::spawn(async move {
            let _ = old.wait().await;
        });
    }

    // Send the single prompt. For Claude we also close the input queue so the
    // query loop finishes; the Codex sidecar hard-exits on stdin EOF, so we must
    // NOT close stdin until the turn is done — `stdin` stays owned below.
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
        // Empty model → a fast default for short translations.
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

    // ── drain: accumulate assistant text until the turn ends ───────────────────
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
            // Turn finished (both sidecars) → we have the full answer.
            Some("result") | Some("_devdy_done") => break,
            _ => {}
        }
    }

    // Reap the child, but only if we're still the current request — a newer
    // request (or a cancel) already took ownership and killed ours.
    if state.generation.load(Ordering::SeqCst) == my_gen {
        if let Some(mut current) = state.child.lock().await.take() {
            let _ = current.start_kill();
            let _ = current.wait().await;
        }
    }
    drop(stdin);

    let out = collected.trim().to_string();
    if out.is_empty() {
        return Err(sidecar_error.unwrap_or_else(|| "Không nhận được bản dịch.".to_string()));
    }
    Ok(out)
}
