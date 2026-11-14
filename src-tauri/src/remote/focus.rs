//! Remote Control Focus actions — the Focus settings (working hours &
//! reminders) — plus the live todo list.
//!
//! Only reached for a Controller that advertised both
//! [`FEATURE_DATA`](crate::remote::protocol::FEATURE_DATA) and
//! [`FEATURE_FOCUS`](crate::remote::protocol::FEATURE_FOCUS) — the gate lives
//! in [`crate::remote::handler::handle`]. Every action calls the same shared
//! function the desktop uses (`focus::settings`), so both surfaces validate
//! identically and the phone gets the desktop's JSON.
//!
//! Failures only ever put a short code on the wire ([`error_code`]); every
//! outcome is audited exactly once.

use crate::focus::settings::{self as focus_settings, FocusSettings};
use crate::remote::audit;
use crate::remote::command::RejectReason;
use crate::remote::data;
use crate::remote::handler::{accept, command_ack, reject, send_payload, HandlerCtx};
use crate::remote::protocol::{CmdPayload, StreamPayload};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Listener};

/// Longest error code put on the wire.
const MAX_CODE_LEN: usize = 40;
/// How long the live todo list waits for a burst of `focus:changed` to settle.
const LIVE_DEBOUNCE: Duration = Duration::from_millis(1_200);

// ---- Pure validation ---------------------------------------------------------

/// The short code a failure is reported with: the desktop error's prefix
/// before `:` when it is a `snake_case` word, `cancelled` / `timeout`, else
/// `failed`. The rest of the text may quote user data or a provider's answer
/// and never leaves the Host.
pub fn error_code(e: &str) -> &str {
    let e = e.trim();
    if matches!(e, "cancelled" | "timeout") {
        return e;
    }
    let is_code = |s: &str| {
        !s.is_empty() && s.len() <= MAX_CODE_LEN && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
    };
    match e.split_once(':') {
        Some((prefix, _)) if is_code(prefix) => prefix,
        _ => "failed",
    }
}

/// Settings only the desktop edits: a phone's copy of them may be stale (they
/// are not pushed when changed on the desktop), so they are ignored on the wire.
pub const DESKTOP_ONLY_SETTINGS: [&str; 4] = ["ai_engine", "ai_model", "calendar_account_id", "calendar_ids"];

/// `current` with the keys of a `focus_settings_set` laid over it, so a phone
/// that does not know a setting leaves it as it is; the
/// [`DESKTOP_ONLY_SETTINGS`] are always kept. An unknown key or a wrongly typed
/// value is malformed; the value rules (`FocusSettings::validated`) are checked
/// by the caller.
pub fn merge_settings(current: &FocusSettings, wire: Option<&Value>) -> Result<FocusSettings, RejectReason> {
    let patch = wire.and_then(Value::as_object).ok_or(RejectReason::Malformed)?;
    let Ok(Value::Object(mut merged)) = serde_json::to_value(current) else {
        return Err(RejectReason::Malformed);
    };
    for (key, val) in patch {
        if !merged.contains_key(key) {
            return Err(RejectReason::Malformed);
        }
        if DESKTOP_ONLY_SETTINGS.contains(&key.as_str()) {
            continue;
        }
        merged.insert(key.clone(), val.clone());
    }
    serde_json::from_value(Value::Object(merged)).map_err(|_| RejectReason::Malformed)
}

// ---- Live todo list ------------------------------------------------------------

/// The `focus:changed` listener of one session and the task that turns it into
/// pushes. Dropped with the session's [`HandlerCtx`].
pub struct LiveTodos {
    app: AppHandle,
    listener: tauri::EventId,
    task: tokio::task::AbortHandle,
}

impl Drop for LiveTodos {
    fn drop(&mut self) {
        self.app.unlisten(self.listener);
        self.task.abort();
    }
}

/// Push a fresh `todo_list` to a `data` + `focus` session whenever a todo
/// changes anywhere (desktop, MCP, sync), coalescing bursts. The task only
/// holds a weak reference: the listener goes away with the session.
pub fn start_live_todos(ctx: &Arc<HandlerCtx>) {
    if !(ctx.data && ctx.focus) {
        return;
    }
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);
    let listener = ctx.app.listen_any(crate::focus::CHANGED_EVENT, move |_| {
        // Full means a push is already due.
        let _ = tx.try_send(());
    });
    let weak = Arc::downgrade(ctx);
    let task = tokio::spawn(async move {
        while rx.recv().await.is_some() {
            tokio::time::sleep(LIVE_DEBOUNCE).await;
            while rx.try_recv().is_ok() {}
            let Some(ctx) = weak.upgrade() else { break };
            data::push_todos(&ctx).await;
        }
    });
    let _ = ctx.live_todos.set(LiveTodos {
        app: ctx.app.clone(),
        listener,
        task: task.abort_handle(),
    });
}

// ---- Dispatch ----------------------------------------------------------------

enum Failure {
    /// Refused before touching anything (bad input, unknown item).
    Rejected(RejectReason),
    /// The shared logic refused or failed: the short code only.
    Failed(String),
}

impl From<RejectReason> for Failure {
    fn from(r: RejectReason) -> Self {
        Failure::Rejected(r)
    }
}

/// Log the full error locally, keep only its code.
fn failed(e: String) -> Failure {
    tracing::debug!(event = "remote_focus_action_failed", error = %e);
    Failure::Failed(error_code(&e).to_string())
}

type FocusResult = Result<(), Failure>;

/// Run one Focus action (already allow-listed, rate-limited and flag-gated).
pub async fn handle(ctx: &Arc<HandlerCtx>, cmd: &CmdPayload) {
    let action = cmd.action.as_str();
    let result = match action {
        "focus_settings_get" => settings_get(ctx).await,
        "focus_settings_set" => settings_set(ctx, cmd).await,
        _ => Err(Failure::Rejected(RejectReason::NotAllowed)),
    };
    match result {
        Ok(()) => {
            accept(ctx, action, None, None).await;
            command_ack(ctx, cmd, true, None).await;
        }
        Err(Failure::Rejected(reason)) => {
            reject(ctx, action, None, reason).await;
            command_ack(ctx, cmd, false, Some(reason.as_str())).await;
        }
        Err(Failure::Failed(code)) => {
            audit::audit(&ctx.db, None, Some(&ctx.room_id), None, action, audit::RESULT_REJECTED, Some(&code))
                .await;
            command_ack(ctx, cmd, false, Some(&code)).await;
        }
    }
}

async fn send_settings(ctx: &HandlerCtx, settings: &FocusSettings) -> Result<(), Failure> {
    let settings = serde_json::to_value(settings).map_err(|e| failed(e.to_string()))?;
    send_payload(ctx, StreamPayload::FocusSettings { settings }).await;
    Ok(())
}

async fn settings_get(ctx: &HandlerCtx) -> FocusResult {
    let settings = focus_settings::load(&ctx.db).await.map_err(failed)?;
    send_settings(ctx, &settings).await
}

/// Save through the desktop's validation, then push the stored settings.
async fn settings_set(ctx: &HandlerCtx, cmd: &CmdPayload) -> FocusResult {
    let current = focus_settings::load(&ctx.db).await.map_err(failed)?;
    let merged = merge_settings(&current, cmd.settings.as_ref())?;
    let valid = merged.validated().map_err(|e| {
        tracing::debug!(event = "remote_focus_settings_invalid", error = %e);
        Failure::Failed("invalid".into())
    })?;
    let saved = focus_settings::save(&ctx.db, valid).await.map_err(failed)?;
    crate::focus::emit_todos_changed(&ctx.app);
    send_settings(ctx, &saved).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn errors_reach_the_wire_as_a_code_only() {
        for (e, code) in [
            ("project_required: pick a project for this todo first", "project_required"),
            ("calendar_failed: Create event failed: 403 token ya29.SECRET", "calendar_failed"),
            ("cancelled", "cancelled"),
            ("timeout", "timeout"),
            (" timeout ", "timeout"),
            // Anything else is opaque.
            ("todo not found: t1", "failed"),
            ("invalid source: empty link", "failed"),
            ("Create event request failed: dns", "failed"),
            ("Busy: x", "failed"),
            ("no-plan: x", "failed"),
            ("secret_token_ghp_abc", "failed"),
            ("", "failed"),
            (": x", "failed"),
        ] {
            assert_eq!(error_code(e), code, "{e}");
        }
        let long = format!("{}: x", "a".repeat(MAX_CODE_LEN + 1));
        assert_eq!(error_code(&long), "failed");
    }

    #[test]
    fn settings_merge_over_the_stored_ones() {
        let current = FocusSettings {
            ai_engine: "codex".into(),
            calendar_account_id: Some("g1".into()),
            ..Default::default()
        };
        let s = merge_settings(
            &current,
            Some(&json!({ "work_start": "08:30", "work_days": [1, 2, 3], "notify_enabled": false })),
        )
        .unwrap();
        assert_eq!(s.work_start, "08:30");
        assert_eq!(s.work_days, vec![1, 2, 3]);
        assert!(!s.notify_enabled);
        // What the phone did not send is kept.
        assert_eq!(s.ai_engine, "codex");
        assert_eq!(s.calendar_account_id.as_deref(), Some("g1"));
        assert_eq!(s.work_end, current.work_end);
        // A phone's (possibly stale) copy of the desktop-only ones is ignored.
        let s = merge_settings(
            &current,
            Some(&json!({ "ai_engine": "claude", "ai_model": "m", "calendar_account_id": null, "calendar_ids": ["x"] })),
        )
        .unwrap();
        assert_eq!(s.ai_engine, "codex");
        assert_eq!(s.calendar_account_id.as_deref(), Some("g1"));
        assert_eq!(s.calendar_ids, current.calendar_ids);

        let bad = |v: Value| merge_settings(&current, Some(&v)).unwrap_err();
        assert_eq!(bad(json!({ "unknown": 1 })), RejectReason::Malformed);
        assert_eq!(bad(json!({ "work_days": "weekdays" })), RejectReason::Malformed);
        assert_eq!(bad(json!({ "notify_enabled": "yes" })), RejectReason::Malformed);
        assert_eq!(bad(json!([1])), RejectReason::Malformed);
        assert_eq!(merge_settings(&current, None).unwrap_err(), RejectReason::Malformed);
        // Value rules are the desktop's: the merge itself accepts this…
        let s = merge_settings(&current, Some(&json!({ "work_end": "07:00" }))).unwrap();
        // …and `validated` (→ `invalid` on the wire) refuses it.
        assert!(s.validated().is_err());
    }

    #[test]
    fn focus_payload_shapes() {
        let v = serde_json::to_value(StreamPayload::FocusSettings {
            settings: serde_json::to_value(FocusSettings::default()).unwrap(),
        })
        .unwrap();
        assert_eq!(v["kind"], "focus_settings");
        assert_eq!(v["settings"]["work_start"], "09:00");
    }

    #[test]
    fn new_inbound_fields_parse() {
        let cmd: CmdPayload = serde_json::from_value(json!({
            "action": "focus_settings_set",
            "settings": { "work_start": "08:00" },
        }))
        .unwrap();
        assert_eq!(cmd.settings, Some(json!({ "work_start": "08:00" })));
        let old: CmdPayload = serde_json::from_value(json!({ "action": "todo_list" })).unwrap();
        assert!(old.settings.is_none());
    }
}
