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
    apply_claude_config_dir, augment_command_path, detach_process_group, kill_process_group,
    resolve_codex_sidecar, resolve_sidecar,
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
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::Mutex as TokioMutex;

/// Cap on how much text we translate in one call — keeps the turn snappy and
/// avoids handing the model a whole transcript by accident.
pub const MAX_INPUT_CHARS: usize = 8_000;

/// Recycle the warm claude session after this many translations to keep the
/// accumulated conversation context (and therefore latency/cost) bounded.
const MAX_TURNS_BEFORE_RECYCLE: u32 = 12;

/// Recycle the warm session once it has been idle for this long — frees the
/// resident sidecar and starts the next translation from a clean context.
const IDLE_RECYCLE: Duration = Duration::from_secs(180);

/// A replaced sidecar that still has turns in flight is kept alive this long at
/// most to finish them, then torn down anyway (its waiters get an error).
const RETIRE_CAP: Duration = Duration::from_secs(120);

/// Turn error messages shown in the UI. Exposed so the Inbox API can map them
/// to its own (English) messages.
pub const ERR_SESSION_ENDED: &str = "Phiên dịch đã kết thúc.";
pub const ERR_EMPTY_RESULT: &str = "Không nhận được bản dịch.";
pub const ERR_FAILED: &str = "Dịch thất bại.";
/// Returned when the text to translate is empty.
pub const ERR_NOTHING_TO_TRANSLATE: &str = "Không có nội dung để dịch.";
/// Prefix of the error returned when the prompt cannot be written.
pub const ERR_SEND_PREFIX: &str = "Không gửi được yêu cầu dịch: ";

/// One step of a turn's output, delivered to in-process callers (the Inbox
/// API) over a channel — mirrors the `translate:chunk|done|error` events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEvent {
    /// Streamed display-only delta.
    Chunk(String),
    /// Authoritative full translation; terminal.
    Done(String),
    /// Turn failed; terminal.
    Error(String),
}

/// Routing state shared between a warm sidecar and its background drain task.
///
/// Turns are pipelined FIFO: `queue` holds the turn ids in submission order and
/// the front entry is the turn currently producing output. The drain tags every
/// emitted event with the front turn id so the frontend can ignore superseded
/// turns, and also forwards it to the turn's channel when one was requested.
#[derive(Default)]
struct TurnRouting {
    /// Turns awaiting / producing output, oldest first, each with an optional
    /// direct channel for in-process callers.
    queue: VecDeque<(u64, Option<UnboundedSender<TurnEvent>>)>,
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
/// Cloning shares the same sidecar (the UI and the Inbox API use one warm
/// session). Other features may run their own instance (e.g. the Slack thread
/// translator) so long jobs never block this one's FIFO.
#[derive(Clone)]
pub struct TranslateState {
    inner: Arc<TokioMutex<Option<WarmSidecar>>>,
    turn_seq: Arc<AtomicU64>,
    /// Recycle a claude session after this many turns.
    recycle_after: u32,
    /// Emit `translate:*` Tauri events (the popover listens to them). Off for
    /// instances whose turn ids would otherwise collide with the popover's.
    emit_events: bool,
}

impl Default for TranslateState {
    fn default() -> Self {
        TranslateState {
            inner: Arc::default(),
            turn_seq: Arc::default(),
            recycle_after: MAX_TURNS_BEFORE_RECYCLE,
            emit_events: true,
        }
    }
}

impl TranslateState {
    /// Recycle the warm claude session after `n` turns (min 1).
    pub fn with_recycle_after(mut self, n: u32) -> Self {
        self.recycle_after = n.max(1);
        self
    }

    /// Do not emit `translate:*` Tauri events; results only reach callers that
    /// asked for a channel (`want_events`).
    pub fn silent(mut self) -> Self {
        self.emit_events = false;
        self
    }

    /// Codex is recycled every turn (its multi-turn semantics aren't relied on);
    /// claude keeps a warm session across several translations.
    fn max_turns(&self, is_codex: bool) -> u32 {
        if is_codex {
            1
        } else {
            self.recycle_after
        }
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

/// Whether a sidecar being replaced must be retired (left to finish its
/// pending turns) rather than killed: only a live one with turns in flight.
fn should_retire(alive: bool, pending: usize) -> bool {
    alive && pending > 0
}

/// Take a replaced sidecar out of service without breaking other callers'
/// turns: a busy, live one is retired (see `retire`), anything else is killed.
async fn replace_out(old: WarmSidecar) {
    let pending = old.routing.lock().await.queue.len();
    if should_retire(old.alive.load(Ordering::SeqCst), pending) {
        retire(old);
    } else {
        teardown(old);
    }
}

/// Let a replaced sidecar finish its queued turns (its drain task keeps routing
/// them), then tear it down — after `RETIRE_CAP` at the latest.
fn retire(w: WarmSidecar) {
    tracing::debug!(event = "translate_sidecar_retired");
    tokio::spawn(async move {
        wait_drained(&w.routing, &w.alive, RETIRE_CAP).await;
        teardown(w);
    });
}

/// Wait until `routing` has no queued turns or the sidecar died. Returns
/// `false` if `cap` elapsed first.
async fn wait_drained(
    routing: &TokioMutex<TurnRouting>,
    alive: &AtomicBool,
    cap: Duration,
) -> bool {
    let deadline = Instant::now() + cap;
    loop {
        if !alive.load(Ordering::SeqCst) || routing.lock().await.queue.is_empty() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Whether `turn` is (still) at the front of the queue, i.e. the turn the
/// sidecar is currently working on.
fn is_front(routing: &TurnRouting, turn: u64) -> bool {
    routing.queue.front().is_some_and(|(t, _)| *t == turn)
}

impl TranslateState {
    /// Kill the warm sidecar (if any) and its CLI process group — app exit,
    /// so no warm session outlives the app.
    pub async fn shutdown(&self) {
        if let Some(w) = self.inner.lock().await.take() {
            teardown(w);
        }
    }

    /// A caller gave up waiting for `turn` (timeout). If it is at the front of
    /// the current sidecar's queue it is considered hung: the sidecar is taken
    /// out of service and torn down right away (its drain then fails the turns
    /// still queued behind it), and the next translation respawns.
    pub async fn abandon_turn(&self, turn: u64) {
        let mut guard = self.inner.lock().await;
        let hung = match guard.as_ref() {
            Some(w) => is_front(&*w.routing.lock().await, turn),
            None => false,
        };
        if hung {
            if let Some(w) = guard.take() {
                tracing::warn!(event = "translate_turn_abandoned", turn = turn);
                teardown(w);
            }
        }
    }
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
    /// Config dir of the default Claude account (None = global profile).
    claude_config_dir: Option<String>,
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
        claude_config_dir: None,
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
    s.claude_config_dir = crate::commands::claude_accounts::default_runtime_account(db)
        .await?
        .map(|a| a.config_dir);
    Ok(s)
}

/// Fingerprint of the settings that pin a warm session — a change respawns it.
fn signature(s: &TranslateSettings) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        s.engine.trim().to_lowercase(),
        s.model.trim(),
        s.claude_path.trim(),
        s.codex_path.trim(),
        s.claude_config_dir.as_deref().unwrap_or("")
    )
}

/// Map a language code/name to a human-readable target name for the prompt.
pub fn target_language_name(code: &str) -> String {
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
pub fn style_instruction(style: &str) -> &'static str {
    match style.trim().to_lowercase().as_str() {
        "literal" => "Translate as literally and faithfully as possible, staying close to the original sentence structure.",
        "technical" => "Keep technical terms, proper nouns, product names, code, and identifiers unchanged; translate only the surrounding prose. Aim for accuracy over fluency.",
        "formal" => "Use a formal, polite register suitable for professional documents.",
        "casual" => "Use a natural, casual, conversational register.",
        // "natural" and anything unknown.
        _ => "Translate naturally and fluently, conveying the meaning rather than a word-for-word rendering.",
    }
}

/// The user's `translate_style` setting (`natural` when unset).
pub async fn load_style(db: &Db) -> Result<String, String> {
    Ok(load_settings(db).await?.style)
}

/// `engine:model` the warm sidecar runs with (claude without a model uses
/// haiku), recorded with what it produced (e.g. Pulse digests).
pub async fn load_engine_model(db: &Db) -> Result<String, String> {
    let s = load_settings(db).await?;
    let engine = s.engine.trim().to_ascii_lowercase();
    let model = match s.model.trim() {
        "" if engine != "codex" => "haiku",
        "" => "default",
        m => m,
    };
    Ok(format!("{engine}:{model}"))
}

/// The user's `translate_target_lang` setting (`vi` when unset).
pub async fn load_default_target(db: &Db) -> Result<String, String> {
    Ok(load_settings(db).await?.default_target)
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

/// Options that open the warm claude session. Every caller of this engine (UI
/// translate, Inbox API, Slack thread translate, Project Pulse digests) feeds it
/// UNTRUSTED text, so the session is tool-less: `noTools` makes the sidecar
/// strip every tool/MCP source and deny any call — a prompt injection in a Slack
/// message or GitHub comment must not reach Bash/Write/WebFetch. Never pair it
/// with `bypassPermissions`, which would auto-approve before any check runs.
fn session_options(model: &str, cwd: &str) -> serde_json::Value {
    let model = match model.trim() {
        "" => "haiku",
        m => m,
    };
    serde_json::json!({
        "cwd": cwd,
        "noTools": true,
        "permissionMode": "dontAsk",
        "allowedTools": [],
        "includePartialMessages": true,
        "model": model,
    })
}

/// Spawn a fresh sidecar for the chosen engine and start its drain task. The
/// returned handle has `started = false`: the caller sends the first prompt
/// (with session options) to actually open the claude query.
async fn spawn_warm(
    app: &AppHandle,
    s: &TranslateSettings,
    is_codex: bool,
    emit_events: bool,
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
        // Tool-less like the claude session (see `session_options`): read-only
        // sandbox, no exec/MCP, every approval declined.
        cmd.env("DEVDY_CODEX_NO_TOOLS", "1");
        cmd.env("DEVDY_PERMISSION_MODE", "plan");
        if !s.model.trim().is_empty() {
            cmd.env("DEVDY_CODEX_MODEL", s.model.trim());
        }
    } else if s.claude_path != "claude" && !s.claude_path.trim().is_empty() {
        cmd.env("DEVDY_CLAUDE_PATH", &s.claude_path);
    }
    if !is_codex {
        apply_claude_config_dir(&mut cmd, s.claude_config_dir.as_deref());
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
        emit_events.then(|| app.clone()),
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
/// boundary. Turns are matched to output FIFO via `routing.queue`. Without an
/// `app` nothing is emitted (turn channels still receive every event).
async fn drain_sidecar<R>(
    app: Option<AppHandle>,
    stdout: R,
    routing: Arc<TokioMutex<TurnRouting>>,
    alive: Arc<AtomicBool>,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    drain_lines(stdout, routing, alive, |turn, ev| {
        if let Some(app) = &app {
            emit_turn_event(app, turn, ev)
        }
    })
    .await;
}

/// Emit a routed turn event as the matching `translate:*` Tauri event.
fn emit_turn_event(app: &AppHandle, turn: u64, ev: &TurnEvent) {
    let _ = match ev {
        TurnEvent::Chunk(delta) => app.emit(
            "translate:chunk",
            serde_json::json!({ "turn": turn, "delta": delta }),
        ),
        TurnEvent::Done(text) => app.emit(
            "translate:done",
            serde_json::json!({ "turn": turn, "text": text }),
        ),
        TurnEvent::Error(err) => app.emit(
            "translate:error",
            serde_json::json!({ "turn": turn, "error": err }),
        ),
    };
}

/// Line loop behind `drain_sidecar`, with event emission injected so the
/// routing can be exercised without an `AppHandle`.
async fn drain_lines<R, F>(
    stdout: R,
    routing: Arc<TokioMutex<TurnRouting>>,
    alive: Arc<AtomicBool>,
    mut emit: F,
) where
    R: tokio::io::AsyncRead + Unpin,
    F: FnMut(u64, &TurnEvent),
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
        let (out, stop) = {
            let mut r = routing.lock().await;
            route_message(&mut r, &v)
        };
        for (turn, ev) in &out {
            emit(*turn, ev);
        }
        if stop {
            break;
        }
    }

    // stdout closed → the sidecar is gone. Fail any turns still in flight so the
    // callers never hang, and mark the handle dead so the next call respawns.
    alive.store(false, Ordering::SeqCst);
    let out = fail_pending(&mut *routing.lock().await);
    for (turn, ev) in &out {
        emit(*turn, ev);
    }
}

/// Deliver `ev` to a turn's direct channel, if it has one. A send error only
/// means the receiver (e.g. a disconnected HTTP client) is gone — ignore it.
fn send_to(tx: Option<&UnboundedSender<TurnEvent>>, ev: &TurnEvent) {
    if let Some(tx) = tx {
        let _ = tx.send(ev.clone());
    }
}

/// Apply one sidecar stdout message to the routing state. Forwards events to
/// the affected turn's channel and returns them (with turn ids) for the caller
/// to emit; the flag is `true` when the session has ended.
fn route_message(r: &mut TurnRouting, v: &Value) -> (Vec<(u64, TurnEvent)>, bool) {
    let mut out = Vec::new();
    match v.get("type").and_then(|x| x.as_str()) {
        // Partial token delta → stream to the popover for the front turn.
        Some("stream_event") => {
            let is_text_delta =
                v.pointer("/event/delta/type").and_then(|t| t.as_str()) == Some("text_delta");
            if is_text_delta {
                if let Some(delta) = v.pointer("/event/delta/text").and_then(|t| t.as_str()) {
                    if !delta.is_empty() {
                        if let Some((turn, tx)) = r.queue.front() {
                            let ev = TurnEvent::Chunk(delta.to_string());
                            send_to(tx.as_ref(), &ev);
                            out.push((*turn, ev));
                        }
                    }
                }
            }
        }
        // Authoritative full text of the answer for the front turn.
        Some("assistant") => {
            if let Some(content) = v.pointer("/message/content").and_then(|c| c.as_array()) {
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
                .unwrap_or(ERR_FAILED)
                .to_string();
            r.error = Some(msg);
        }
        // Turn finished → resolve the front turn.
        Some("result") => {
            if let Some((turn, tx)) = r.queue.pop_front() {
                let ev = if let Some(err) = r.error.take() {
                    TurnEvent::Error(err)
                } else {
                    let text = r.final_text.trim().to_string();
                    if text.is_empty() {
                        TurnEvent::Error(ERR_EMPTY_RESULT.to_string())
                    } else {
                        TurnEvent::Done(text)
                    }
                };
                send_to(tx.as_ref(), &ev);
                out.push((turn, ev));
            }
            r.final_text.clear();
            r.error = None;
        }
        // Session torn down (Codex single turn, or claude session death).
        Some("_devdy_done") | Some("_devdy_closed") => return (out, true),
        _ => {}
    }
    (out, false)
}

/// Fail every turn still queued (the sidecar's stdout closed).
fn fail_pending(r: &mut TurnRouting) -> Vec<(u64, TurnEvent)> {
    let pending: Vec<_> = r.queue.drain(..).collect();
    let err = r
        .error
        .take()
        .unwrap_or_else(|| ERR_SESSION_ENDED.to_string());
    pending
        .into_iter()
        .map(|(turn, tx)| {
            let ev = TurnEvent::Error(err.clone());
            send_to(tx.as_ref(), &ev);
            (turn, ev)
        })
        .collect()
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
            replace_out(old).await;
        }
        let w = spawn_warm(&app, &settings, is_codex, state.emit_events).await?;
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
    let source: String = text.trim().chars().take(MAX_INPUT_CHARS).collect();
    let (turn_id, _, _) = submit(&app, &db, &state, source, target_lang, false).await?;
    Ok(turn_id)
}

/// Core of a translation turn, shared by the Tauri command and the Inbox API.
/// Ensures a warm sidecar, enqueues the turn, writes the prompt and returns
/// `(turn id, event channel when `want_events`, resolved target language)`.
/// The `translate:*` events are emitted either way. Engine, model and style
/// always come from settings; only the target language can be overridden.
/// Does not truncate — callers enforce their own length limit.
pub async fn submit(
    app: &AppHandle,
    db: &Db,
    state: &TranslateState,
    text: String,
    target_lang: Option<String>,
    want_events: bool,
) -> Result<(u64, Option<UnboundedReceiver<TurnEvent>>, String), String> {
    let source = text.trim();
    if source.is_empty() {
        return Err(ERR_NOTHING_TO_TRANSLATE.to_string());
    }

    let settings = load_settings(db).await?;
    let target = target_lang
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| settings.default_target.clone());
    let target_name = target_language_name(&target);
    let prompt = build_prompt(source, &target_name, &settings.style);
    let (turn_id, rx) = submit_with(app, state, &settings, prompt, want_events).await?;
    Ok((turn_id, rx, target))
}

/// Submit a ready-made prompt (no wrapping, no target language) as one turn
/// on `state`'s warm sidecar. Engine/model/account come from settings. Returns
/// `(turn id, event channel when want_events)`.
pub async fn submit_prompt(
    app: &AppHandle,
    db: &Db,
    state: &TranslateState,
    prompt: String,
    want_events: bool,
) -> Result<(u64, Option<UnboundedReceiver<TurnEvent>>), String> {
    if prompt.trim().is_empty() {
        return Err(ERR_NOTHING_TO_TRANSLATE.to_string());
    }
    let settings = load_settings(db).await?;
    submit_with(app, state, &settings, prompt, want_events).await
}

/// Shared body of `submit` / `submit_prompt`: ensure a warm sidecar, enqueue
/// the turn and write the prompt.
async fn submit_with(
    app: &AppHandle,
    state: &TranslateState,
    settings: &TranslateSettings,
    prompt: String,
    want_events: bool,
) -> Result<(u64, Option<UnboundedReceiver<TurnEvent>>), String> {
    let sig = signature(settings);
    let is_codex = settings.engine.trim().eq_ignore_ascii_case("codex");

    let turn_id = state.turn_seq.fetch_add(1, Ordering::SeqCst) + 1;

    let mut guard = state.inner.lock().await;

    // Recycle the warm session if it died, settings changed, it has served
    // enough turns, or it has gone stale — otherwise reuse it. A live session
    // with turns still in flight is retired, not killed, so those turns (other
    // callers') finish; new turns go to the fresh sidecar.
    let recycle = match guard.as_ref() {
        None => true,
        Some(w) => {
            !w.alive.load(Ordering::SeqCst)
                || w.signature != sig
                || w.turns >= state.max_turns(w.is_codex)
                || w.last_used.elapsed() > IDLE_RECYCLE
        }
    };
    if recycle {
        if let Some(old) = guard.take() {
            replace_out(old).await;
        }
        let w = spawn_warm(app, settings, is_codex, state.emit_events).await?;
        *guard = Some(w);
    }

    let w = guard.as_mut().ok_or("translate sidecar unavailable")?;

    // Turns are pipelined FIFO on the shared session — a superseded turn simply
    // finishes and is ignored by the caller (it filters events by turn id). We
    // deliberately do NOT interrupt the in-flight turn: it may belong to another
    // caller (e.g. a calendar batch) sharing this warm sidecar.

    // Enqueue BEFORE writing so the drain routes this turn's output correctly.
    let (tx, rx) = if want_events {
        let (tx, rx) = unbounded_channel();
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };
    w.routing.lock().await.queue.push_back((turn_id, tx));

    // First prompt carries the session options; follow-ups are bare user turns.
    let line = if !w.started && !is_codex {
        let options = session_options(&settings.model, &std::env::temp_dir().to_string_lossy());
        serde_json::json!({ "type": "prompt", "text": prompt, "options": options }).to_string()
    } else {
        serde_json::json!({ "type": "prompt", "text": prompt }).to_string()
    };

    if let Err(e) = w.stdin.write_all(format!("{line}\n").as_bytes()).await {
        // The pipe died between the liveness check and the write — drop the turn
        // and surface a clean error; the next call will respawn.
        w.routing.lock().await.queue.pop_back();
        w.alive.store(false, Ordering::SeqCst);
        return Err(format!("{ERR_SEND_PREFIX}{e}"));
    }
    let _ = w.stdin.flush().await;
    w.started = true;
    w.turns += 1;
    w.last_used = Instant::now();

    Ok((turn_id, rx))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_options_are_tool_less() {
        let o = session_options("  ", "/tmp");
        assert_eq!(o["noTools"], true);
        assert_eq!(o["permissionMode"], "dontAsk");
        assert_eq!(o["allowedTools"], serde_json::json!([]));
        assert!(o.get("mcpServers").is_none());
        assert_eq!(o["model"], "haiku");
        assert_eq!(o["cwd"], "/tmp");
        assert_eq!(session_options(" sonnet ", "/tmp")["model"], "sonnet");
        // Never auto-approve: bypass would skip every permission check.
        assert!(!o.to_string().contains("bypassPermissions"));
    }

    fn delta(text: &str) -> String {
        serde_json::json!({
            "type": "stream_event",
            "event": { "delta": { "type": "text_delta", "text": text } }
        })
        .to_string()
    }

    fn assistant(text: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "message": { "content": [{ "type": "text", "text": text }] }
        })
        .to_string()
    }

    /// Queue `turns` (each with a channel), drain `lines` from an in-memory
    /// reader, and return the receivers plus every emitted (turn, event).
    async fn drive(
        turns: &[u64],
        lines: &[String],
    ) -> (
        Vec<UnboundedReceiver<TurnEvent>>,
        Vec<(u64, TurnEvent)>,
        bool,
    ) {
        let routing = Arc::new(TokioMutex::new(TurnRouting::default()));
        let mut rxs = Vec::new();
        for &t in turns {
            let (tx, rx) = unbounded_channel();
            routing.lock().await.queue.push_back((t, Some(tx)));
            rxs.push(rx);
        }
        let alive = Arc::new(AtomicBool::new(true));
        let input = lines.join("\n").into_bytes();
        let mut emitted = Vec::new();
        drain_lines(&input[..], routing, alive.clone(), |turn, ev| {
            emitted.push((turn, ev.clone()))
        })
        .await;
        (rxs, emitted, alive.load(Ordering::SeqCst))
    }

    fn collect(rx: &mut UnboundedReceiver<TurnEvent>) -> Vec<TurnEvent> {
        let mut v = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            v.push(ev);
        }
        v
    }

    #[tokio::test]
    async fn drain_delivers_chunks_then_done_to_the_front_turn() {
        let lines = vec![
            "not json".to_string(),
            delta("Xin "),
            delta("chào"),
            assistant("Xin chào"),
            r#"{"type":"result"}"#.to_string(),
            delta("Hi"),
            assistant("Hi"),
            r#"{"type":"result"}"#.to_string(),
        ];
        let (mut rxs, emitted, alive) = drive(&[1, 2], &lines).await;
        assert_eq!(
            collect(&mut rxs[0]),
            vec![
                TurnEvent::Chunk("Xin ".into()),
                TurnEvent::Chunk("chào".into()),
                TurnEvent::Done("Xin chào".into()),
            ]
        );
        assert_eq!(
            collect(&mut rxs[1]),
            vec![TurnEvent::Chunk("Hi".into()), TurnEvent::Done("Hi".into())]
        );
        // The same events are emitted (for the Tauri UI), tagged by turn.
        assert_eq!(emitted.len(), 5);
        assert_eq!(emitted[2], (1, TurnEvent::Done("Xin chào".into())));
        assert_eq!(emitted[4], (2, TurnEvent::Done("Hi".into())));
        // stdout closed afterwards → handle marked dead, nothing left to fail.
        assert!(!alive);
    }

    #[tokio::test]
    async fn drain_reports_sidecar_error_at_result() {
        let lines = vec![
            delta("partial"),
            r#"{"type":"_devdy_error","error":"boom"}"#.to_string(),
            r#"{"type":"result"}"#.to_string(),
        ];
        let (mut rxs, _, _) = drive(&[7], &lines).await;
        assert_eq!(
            collect(&mut rxs[0]),
            vec![
                TurnEvent::Chunk("partial".into()),
                TurnEvent::Error("boom".into()),
            ]
        );
    }

    #[tokio::test]
    async fn drain_fails_pending_turns_when_stdout_closes() {
        let lines = vec![
            delta("a"),
            r#"{"type":"_devdy_closed"}"#.to_string(),
            delta("ignored"),
        ];
        let (mut rxs, emitted, alive) = drive(&[1, 2], &lines).await;
        assert_eq!(
            collect(&mut rxs[0]),
            vec![
                TurnEvent::Chunk("a".into()),
                TurnEvent::Error(ERR_SESSION_ENDED.into()),
            ]
        );
        assert_eq!(
            collect(&mut rxs[1]),
            vec![TurnEvent::Error(ERR_SESSION_ENDED.into())]
        );
        assert_eq!(emitted.len(), 3);
        assert!(!alive);
    }

    #[test]
    fn dropped_receiver_does_not_break_routing() {
        let mut r = TurnRouting::default();
        let (tx, rx) = unbounded_channel();
        drop(rx);
        r.queue.push_back((1, Some(tx)));
        r.queue.push_back((2, None));
        let (out, stop) = route_message(&mut r, &serde_json::from_str(&delta("x")).unwrap());
        assert!(!stop);
        assert_eq!(out, vec![(1, TurnEvent::Chunk("x".into()))]);
        let empty_result: Value = serde_json::json!({ "type": "result" });
        let (out, _) = route_message(&mut r, &empty_result);
        assert_eq!(out, vec![(1, TurnEvent::Error(ERR_EMPTY_RESULT.into()))]);
        assert_eq!(r.queue.len(), 1);
    }

    #[tokio::test]
    async fn interleaved_channel_and_event_only_turns_stay_fifo() {
        // UI turn (no channel) between two API turns.
        let routing = Arc::new(TokioMutex::new(TurnRouting::default()));
        let (tx1, mut rx1) = unbounded_channel();
        let (tx3, mut rx3) = unbounded_channel();
        {
            let mut r = routing.lock().await;
            r.queue.push_back((1, Some(tx1)));
            r.queue.push_back((2, None));
            r.queue.push_back((3, Some(tx3)));
        }
        let lines = [
            delta("one"),
            assistant("one"),
            r#"{"type":"result"}"#.to_string(),
            delta("two"),
            assistant("two"),
            r#"{"type":"result"}"#.to_string(),
            delta("three"),
            assistant("three"),
            r#"{"type":"result"}"#.to_string(),
        ]
        .join("\n");
        let mut emitted = Vec::new();
        drain_lines(
            lines.as_bytes(),
            routing,
            Arc::new(AtomicBool::new(true)),
            |turn, ev| emitted.push((turn, ev.clone())),
        )
        .await;
        assert_eq!(
            collect(&mut rx1),
            vec![
                TurnEvent::Chunk("one".into()),
                TurnEvent::Done("one".into())
            ]
        );
        assert_eq!(
            collect(&mut rx3),
            vec![
                TurnEvent::Chunk("three".into()),
                TurnEvent::Done("three".into())
            ]
        );
        assert!(emitted.contains(&(2, TurnEvent::Chunk("two".into()))));
        assert!(emitted.contains(&(2, TurnEvent::Done("two".into()))));
    }

    #[test]
    fn recycle_limit_is_per_instance() {
        let shared = TranslateState::default();
        assert_eq!(shared.max_turns(false), MAX_TURNS_BEFORE_RECYCLE);
        assert!(shared.emit_events);
        let slack = TranslateState::default().with_recycle_after(6).silent();
        assert_eq!(slack.max_turns(false), 6);
        assert_eq!(slack.max_turns(true), 1);
        assert!(!slack.emit_events);
    }

    #[test]
    fn only_busy_live_sidecars_are_retired() {
        assert!(should_retire(true, 1));
        assert!(!should_retire(true, 0));
        // Dead sidecars are killed even with turns queued (their drain has
        // already failed them, or teardown will).
        assert!(!should_retire(false, 3));
    }

    #[tokio::test]
    async fn retired_sidecar_finishes_its_pending_turns_before_teardown() {
        // A retired sidecar's drain keeps routing its queue; `wait_drained`
        // returns once that queue is empty, with the session still alive.
        let routing = Arc::new(TokioMutex::new(TurnRouting::default()));
        let (tx, mut rx) = unbounded_channel();
        routing.lock().await.queue.push_back((5, Some(tx)));
        let alive = Arc::new(AtomicBool::new(true));
        let (mut writer, reader) = tokio::io::duplex(4096);
        let drain = tokio::spawn(drain_lines(
            reader,
            routing.clone(),
            alive.clone(),
            |_, _| {},
        ));

        let waiter = {
            let (routing, alive) = (routing.clone(), alive.clone());
            tokio::spawn(async move { wait_drained(&routing, &alive, RETIRE_CAP).await })
        };
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(!waiter.is_finished());

        let out = format!(
            "{}\n{}\n{{\"type\":\"result\"}}\n",
            delta("ok"),
            assistant("ok")
        );
        writer.write_all(out.as_bytes()).await.unwrap();
        assert!(waiter.await.unwrap());
        assert!(alive.load(Ordering::SeqCst));
        assert_eq!(
            collect(&mut rx),
            vec![TurnEvent::Chunk("ok".into()), TurnEvent::Done("ok".into())]
        );
        drop(writer);
        drain.await.unwrap();
    }

    #[tokio::test]
    async fn wait_drained_stops_on_death_or_cap() {
        let routing = TokioMutex::new(TurnRouting::default());
        routing.lock().await.queue.push_back((1, None));
        let alive = AtomicBool::new(true);
        assert!(!wait_drained(&routing, &alive, Duration::from_millis(50)).await);
        alive.store(false, Ordering::SeqCst);
        assert!(wait_drained(&routing, &alive, Duration::from_secs(5)).await);
    }

    #[test]
    fn only_the_front_turn_counts_as_hung() {
        let mut r = TurnRouting::default();
        assert!(!is_front(&r, 1));
        r.queue.push_back((1, None));
        r.queue.push_back((2, None));
        assert!(is_front(&r, 1));
        assert!(!is_front(&r, 2));
    }

    /// A resident `cat` stands in for the sidecar: it stays alive and silent
    /// until killed, like a sidecar stuck on a hung turn.
    #[cfg(unix)]
    async fn fake_warm(turns: &[u64]) -> (WarmSidecar, Vec<UnboundedReceiver<TurnEvent>>) {
        let mut cmd = tokio::process::Command::new("cat");
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped());
        detach_process_group(&mut cmd);
        cmd.kill_on_drop(true);
        let mut child = cmd.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stdin = child.stdin.take().unwrap();
        let routing = Arc::new(TokioMutex::new(TurnRouting::default()));
        let mut rxs = Vec::new();
        for &t in turns {
            let (tx, rx) = unbounded_channel();
            routing.lock().await.queue.push_back((t, Some(tx)));
            rxs.push(rx);
        }
        let alive = Arc::new(AtomicBool::new(true));
        tokio::spawn(drain_lines(
            stdout,
            routing.clone(),
            alive.clone(),
            |_, _| {},
        ));
        let w = WarmSidecar {
            child,
            stdin,
            signature: String::new(),
            is_codex: false,
            started: true,
            turns: turns.len() as u32,
            last_used: Instant::now(),
            routing,
            alive,
        };
        (w, rxs)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn abandoning_the_hung_front_turn_tears_the_sidecar_down() {
        let state = TranslateState::default();
        let (w, mut rxs) = fake_warm(&[1, 2]).await;
        let alive = w.alive.clone();
        *state.inner.lock().await = Some(w);

        // Not the front turn → nothing happens.
        state.abandon_turn(2).await;
        assert!(state.inner.lock().await.is_some());
        assert!(alive.load(Ordering::SeqCst));

        state.abandon_turn(1).await;
        assert!(state.inner.lock().await.is_none());
        // Killing the process closes stdout → every queued turn is failed.
        for rx in rxs.iter_mut() {
            let ev = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("turn failed after teardown");
            assert_eq!(ev, Some(TurnEvent::Error(ERR_SESSION_ENDED.into())));
        }
        assert!(!alive.load(Ordering::SeqCst));

        // Stale turn id (sidecar already gone) → no-op.
        state.abandon_turn(1).await;
        assert!(state.inner.lock().await.is_none());
    }
}
