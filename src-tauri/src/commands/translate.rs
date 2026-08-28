//! Translate — fast LLM translation of a short text selection.
//!
//! The "AI result" view lets the user select any text and translate it. Rather
//! than routing through the full run machinery (a persistent, multi-turn session
//! with permission gating), this module keeps ONE **warm** Agent-SDK sidecar
//! alive and reuses it across translations, so only the first translation pays
//! the cold-start cost (Node boot + SDK import + `claude` CLI spawn). Every
//! subsequent translation is a cheap follow-up turn on the already-open session.
//!
//! Speed comes from three things:
//!   1. **Warm reuse** — the sidecar (and its claude session) is not torn down
//!      between translations; the process stays resident (`TranslateState`).
//!   2. **Prewarm** — `prewarm_translate` spawns the sidecar as soon as the
//!      floating "Translate" button appears, hiding the Node/import boot behind
//!      the user moving the mouse to click.
//!   3. **Streaming** — the translation is streamed to the popover token by token
//!      (`includePartialMessages: true` + `translate:chunk` events), so the user
//!      sees words appear instead of waiting for the whole answer.
//!
//! Because a reused claude session accumulates context, the warm session is
//! recycled after `MAX_TURNS_BEFORE_RECYCLE` translations or after `IDLE_RECYCLE`
//! of inactivity (or immediately when engine/model settings change). Codex is
//! recycled every turn — the Codex sidecar's multi-turn behaviour is not relied
//! on here — so Codex keeps its previous one-shot semantics but still streams.
//!
//! Engine/model/style come from the user's settings (`translate_engine`,
//! `translate_model`, `translate_style`, `translate_target_lang`); the caller may
//! override the target language per call (the popover's quick toggle).
//!
//! Auth is inherited exactly like a normal run: the SDK/CLI reads the same
//! Keychain / ChatGPT login, so translations bill against the subscription with
//! no API key.

use crate::db::Db;
use crate::runs::sidecar::{
    augment_command_path, detach_process_group, kill_process_group, resolve_codex_sidecar,
    resolve_sidecar,
};
use serde_json::Value;
use sqlx::Row;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::ChildStdin;
use tokio::sync::Mutex as TokioMutex;

/// Cap on how much text we translate in one call — keeps the turn snappy and
/// avoids handing the model a whole transcript by accident.
const MAX_INPUT_CHARS: usize = 8_000;

/// Recycle the warm claude session after this many translations to keep the
/// accumulated conversation context (and therefore latency/cost) bounded.
const MAX_TURNS_BEFORE_RECYCLE: u32 = 12;

/// Recycle the warm session once it has been idle for this long — frees the
/// resident sidecar and starts the next translation from a clean context.
const IDLE_RECYCLE: Duration = Duration::from_secs(180);

/// Routing state shared between a warm sidecar and its background drain task.
///
/// Turns are pipelined FIFO: `queue` holds the turn ids in submission order and
/// `front` is the turn currently producing output. The drain tags every emitted
/// event with the front turn id so the frontend can ignore superseded turns.
#[derive(Default)]
struct TurnRouting {
    /// Turn ids awaiting / producing output, oldest first.
    queue: VecDeque<u64>,
    /// Authoritative text of the front turn, accumulated from `assistant`
    /// messages (the streamed `translate:chunk` deltas are display-only).
    final_text: String,
    /// Error reported by the sidecar for the front turn, if any.
    error: Option<String>,
}

/// A resident translation sidecar plus everything needed to route turns to it.
struct WarmSidecar {
    child: tokio::process::Child,
    stdin: ChildStdin,
    /// engine/model/binary fingerprint — a change forces a respawn.
    signature: String,
    is_codex: bool,
    /// Whether the claude query session has been started (first prompt sent).
    /// The first prompt carries the session options; later prompts are bare
    /// follow-up user turns.
    started: bool,
    /// Translations served by this session so far (drives recycling).
    turns: u32,
    last_used: Instant,
    routing: Arc<TokioMutex<TurnRouting>>,
    /// Cleared by the drain task when the sidecar's stdout closes / the session
    /// ends, so the next call knows to respawn instead of writing to a dead pipe.
    alive: Arc<AtomicBool>,
}

/// Holds the single warm translation sidecar (if any) and hands out turn ids.
#[derive(Default)]
pub struct TranslateState {
    inner: Arc<TokioMutex<Option<WarmSidecar>>>,
    turn_seq: Arc<AtomicU64>,
}

/// Codex is recycled every turn (its multi-turn semantics aren't relied on);
/// claude keeps a warm session across several translations.
fn max_turns(is_codex: bool) -> u32 {
    if is_codex {
        1
    } else {
        MAX_TURNS_BEFORE_RECYCLE
    }
}

/// Kill a warm sidecar and its CLI grandchild, reaping asynchronously.
fn teardown(mut w: WarmSidecar) {
    w.alive.store(false, Ordering::SeqCst);
    let pid = w.child.id();
    let _ = w.child.start_kill();
    if let Some(pid) = pid {
        kill_process_group(pid);
    }
    tokio::spawn(async move {
        let _ = w.child.wait().await;
    });
}

/// Settings needed to spawn / drive the translation sidecar.
struct TranslateSettings {
    node_path: String,
    sidecar_path: String,
    codex_sidecar_path: String,
    claude_path: String,
    codex_path: String,
    engine: String,
    model: String,
    style: String,
    default_target: String,
}

async fn load_settings(db: &Db) -> Result<TranslateSettings, String> {
    let rows = sqlx::query("SELECT key, value FROM settings")
        .fetch_all(db)
        .await
        .map_err(|e| e.to_string())?;
    let mut s = TranslateSettings {
        node_path: "node".to_string(),
        sidecar_path: String::new(),
        codex_sidecar_path: String::new(),
        claude_path: String::new(),
        codex_path: String::new(),
        engine: "claude".to_string(),
        model: String::new(),
        style: "natural".to_string(),
        default_target: "vi".to_string(),
    };
    for row in &rows {
        let key: String = row.get("key");
        let value: String = row.get("value");
        match key.as_str() {
            "node_path" => s.node_path = value,
            "sidecar_path" => s.sidecar_path = value,
            "codex_sidecar_path" => s.codex_sidecar_path = value,
            "claude_path" => s.claude_path = value,
            "codex_path" => s.codex_path = value,
            "translate_engine" if !value.trim().is_empty() => s.engine = value,
            "translate_model" => s.model = value,
            "translate_style" if !value.trim().is_empty() => s.style = value,
            "translate_target_lang" if !value.trim().is_empty() => s.default_target = value,
            _ => {}
        }
    }
    Ok(s)
}

/// Fingerprint of the settings that pin a warm session — a change respawns it.
fn signature(s: &TranslateSettings) -> String {
    format!(
        "{}|{}|{}|{}",
        s.engine.trim().to_lowercase(),
        s.model.trim(),
        s.claude_path.trim(),
        s.codex_path.trim()
    )
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

fn build_prompt(source: &str, target_name: &str, style: &str) -> String {
    format!(
        "You are a professional translator. Translate the text delimited by <<<>>> into {target_name}.\n\
         {style}\n\
         Output ONLY the translation — no preamble, no explanation, no quotes, no notes. \
         Preserve markdown formatting, code blocks, and inline code unchanged. If the text is \
         already in {target_name}, return it unchanged.\n\n\
         <<<\n{source}\n>>>",
        target_name = target_name,
        style = style_instruction(style),
        source = source,
    )
}

/// Spawn a fresh sidecar for the chosen engine and start its drain task. The
/// returned handle has `started = false`: the caller sends the first prompt
/// (with session options) to actually open the claude query.
async fn spawn_warm(
    app: &AppHandle,
    s: &TranslateSettings,
    is_codex: bool,
) -> Result<WarmSidecar, String> {
    let (node_bin, sidecar_script) = if is_codex {
        resolve_codex_sidecar(app, &s.node_path, &s.codex_sidecar_path)?
    } else {
        resolve_sidecar(app, &s.node_path, &s.sidecar_path)?
    };
    let cwd = std::env::temp_dir();
    let mut cmd = tokio::process::Command::new(&node_bin);
    cmd.current_dir(&cwd).arg(&sidecar_script);
    augment_command_path(&mut cmd);
    if is_codex {
        if s.codex_path != "codex" && !s.codex_path.trim().is_empty() {
            cmd.env("DEVDY_CODEX_PATH", &s.codex_path);
        }
        cmd.env("DEVDY_PERMISSION_MODE", "bypassPermissions");
        if !s.model.trim().is_empty() {
            cmd.env("DEVDY_CODEX_MODEL", s.model.trim());
        }
    } else if s.claude_path != "claude" && !s.claude_path.trim().is_empty() {
        cmd.env("DEVDY_CLAUDE_PATH", &s.claude_path);
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
    let stdin = child.stdin.take().ok_or("no stdin")?;

    let routing = Arc::new(TokioMutex::new(TurnRouting::default()));
    let alive = Arc::new(AtomicBool::new(true));

    tokio::spawn(drain_sidecar(
        app.clone(),
        stdout,
        routing.clone(),
        alive.clone(),
    ));

    Ok(WarmSidecar {
        child,
        stdin,
        signature: signature(s),
        is_codex,
        started: false,
        turns: 0,
        last_used: Instant::now(),
        routing,
        alive,
    })
}

/// Read the sidecar's stdout forever, streaming `translate:chunk` deltas and
/// resolving each turn (`translate:done` / `translate:error`) at its `result`
/// boundary. Turns are matched to output FIFO via `routing.queue`.
async fn drain_sidecar<R>(
    app: AppHandle,
    stdout: R,
    routing: Arc<TokioMutex<TurnRouting>>,
    alive: Arc<AtomicBool>,
) where
    R: tokio::io::AsyncRead + Unpin,
{
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
            // Partial token delta → stream to the popover for the front turn.
            Some("stream_event") => {
                let is_text_delta = v.pointer("/event/delta/type").and_then(|t| t.as_str())
                    == Some("text_delta");
                if is_text_delta {
                    if let Some(delta) = v.pointer("/event/delta/text").and_then(|t| t.as_str()) {
                        if !delta.is_empty() {
                            let front = { routing.lock().await.queue.front().copied() };
                            if let Some(turn) = front {
                                let _ = app.emit(
                                    "translate:chunk",
                                    serde_json::json!({ "turn": turn, "delta": delta }),
                                );
                            }
                        }
                    }
                }
            }
            // Authoritative full text of the answer for the front turn.
            Some("assistant") => {
                if let Some(content) = v.pointer("/message/content").and_then(|c| c.as_array()) {
                    let mut r = routing.lock().await;
                    for block in content {
                        if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                            if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                                r.final_text.push_str(t);
                            }
                        }
                    }
                }
            }
            Some("_devdy_error") => {
                let msg = v
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("Dịch thất bại.")
                    .to_string();
                routing.lock().await.error = Some(msg);
            }
            // Turn finished → resolve the front turn.
            Some("result") => {
                let mut r = routing.lock().await;
                if let Some(turn) = r.queue.pop_front() {
                    if let Some(err) = r.error.take() {
                        let _ = app.emit(
                            "translate:error",
                            serde_json::json!({ "turn": turn, "error": err }),
                        );
                    } else {
                        let text = r.final_text.trim().to_string();
                        if text.is_empty() {
                            let _ = app.emit(
                                "translate:error",
                                serde_json::json!({ "turn": turn, "error": "Không nhận được bản dịch." }),
                            );
                        } else {
                            let _ = app.emit(
                                "translate:done",
                                serde_json::json!({ "turn": turn, "text": text }),
                            );
                        }
                    }
                }
                r.final_text.clear();
                r.error = None;
            }
            // Session torn down (Codex single turn, or claude session death).
            Some("_devdy_done") | Some("_devdy_closed") => break,
            _ => {}
        }
    }

    // stdout closed → the sidecar is gone. Fail any turns still in flight so the
    // frontend never hangs, and mark the handle dead so the next call respawns.
    alive.store(false, Ordering::SeqCst);
    let mut r = routing.lock().await;
    let pending: Vec<u64> = r.queue.drain(..).collect();
    let err = r
        .error
        .take()
        .unwrap_or_else(|| "Phiên dịch đã kết thúc.".to_string());
    drop(r);
    for turn in pending {
        let _ = app.emit(
            "translate:error",
            serde_json::json!({ "turn": turn, "error": err }),
        );
    }
}

/// Spawn (or reuse) the warm sidecar ahead of an actual translation, so the
/// Node boot + SDK import is paid while the user is still reaching for the
/// button. No prompt is sent — the claude session opens on the first real turn.
#[tauri::command]
pub async fn prewarm_translate(
    app: AppHandle,
    db: State<'_, Db>,
    state: State<'_, TranslateState>,
) -> Result<(), String> {
    let settings = load_settings(&db).await?;
    let sig = signature(&settings);
    let is_codex = settings.engine.trim().eq_ignore_ascii_case("codex");

    let mut guard = state.inner.lock().await;
    let reusable = matches!(
        guard.as_ref(),
        Some(w) if w.alive.load(Ordering::SeqCst) && w.signature == sig
    );
    if !reusable {
        if let Some(old) = guard.take() {
            teardown(old);
        }
        let w = spawn_warm(&app, &settings, is_codex).await?;
        *guard = Some(w);
    }
    Ok(())
}

/// Start a translation turn. Ensures a warm sidecar, writes the prompt, and
/// returns the turn id immediately — the translation streams back via
/// `translate:chunk` / `translate:done` / `translate:error` events, all tagged
/// with this turn id so the caller can ignore superseded turns.
#[tauri::command]
pub async fn translate_text(
    app: AppHandle,
    db: State<'_, Db>,
    state: State<'_, TranslateState>,
    text: String,
    target_lang: Option<String>,
) -> Result<u64, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("Không có nội dung để dịch.".to_string());
    }
    let source: String = trimmed.chars().take(MAX_INPUT_CHARS).collect();

    let settings = load_settings(&db).await?;
    let sig = signature(&settings);
    let is_codex = settings.engine.trim().eq_ignore_ascii_case("codex");

    let target = target_lang
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| settings.default_target.clone());
    let target_name = target_language_name(&target);
    let prompt = build_prompt(&source, &target_name, &settings.style);

    let turn_id = state.turn_seq.fetch_add(1, Ordering::SeqCst) + 1;

    let mut guard = state.inner.lock().await;

    // Recycle the warm session if it died, settings changed, it has served
    // enough turns, or it has gone stale — otherwise reuse it.
    let recycle = match guard.as_ref() {
        None => true,
        Some(w) => {
            !w.alive.load(Ordering::SeqCst)
                || w.signature != sig
                || w.turns >= max_turns(w.is_codex)
                || w.last_used.elapsed() > IDLE_RECYCLE
        }
    };
    if recycle {
        if let Some(old) = guard.take() {
            teardown(old);
        }
        let w = spawn_warm(&app, &settings, is_codex).await?;
        *guard = Some(w);
    }

    let w = guard.as_mut().ok_or("translate sidecar unavailable")?;

    // Turns are pipelined FIFO on the shared session — a superseded turn simply
    // finishes and is ignored by the caller (it filters events by turn id). We
    // deliberately do NOT interrupt the in-flight turn: it may belong to another
    // caller (e.g. a calendar batch) sharing this warm sidecar.

    // Enqueue BEFORE writing so the drain routes this turn's output correctly.
    w.routing.lock().await.queue.push_back(turn_id);

    // First prompt carries the session options; follow-ups are bare user turns.
    let line = if !w.started && !is_codex {
        let m = if settings.model.trim().is_empty() {
            "haiku".to_string()
        } else {
            settings.model.trim().to_string()
        };
        let options = serde_json::json!({
            "cwd": std::env::temp_dir().to_string_lossy(),
            "permissionMode": "bypassPermissions",
            "allowedTools": [],
            "includePartialMessages": true,
            "model": m,
        });
        serde_json::json!({ "type": "prompt", "text": prompt, "options": options }).to_string()
    } else {
        serde_json::json!({ "type": "prompt", "text": prompt }).to_string()
    };

    if let Err(e) = w.stdin.write_all(format!("{line}\n").as_bytes()).await {
        // The pipe died between the liveness check and the write — drop the turn
        // and surface a clean error; the next call will respawn.
        w.routing.lock().await.queue.pop_back();
        w.alive.store(false, Ordering::SeqCst);
        return Err(format!("Không gửi được yêu cầu dịch: {e}"));
    }
    let _ = w.stdin.flush().await;
    w.started = true;
    w.turns += 1;
    w.last_used = Instant::now();

    Ok(turn_id)
}

/// Called when the popover closes. The warm sidecar is intentionally kept hot
/// for the next selection, so this is a no-op: any turn still streaming is
/// harmless — the (now unmounted) caller ignores its events by turn id, and the
/// session is recycled later by turn count / idle timeout. Interrupting or
/// tearing down here would risk killing another caller's turn on the shared
/// sidecar (e.g. a concurrent calendar batch).
#[tauri::command]
pub async fn cancel_translate(_state: State<'_, TranslateState>) -> Result<(), String> {
    Ok(())
}
