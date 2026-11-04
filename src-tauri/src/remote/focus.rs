//! Remote Control Focus actions: the board, Focus AI, day plans, "Start with
//! AI", BR-11 actual time and the Focus settings, plus the live board.
//!
//! Only reached for a Controller that advertised both
//! [`FEATURE_DATA`](crate::remote::protocol::FEATURE_DATA) and
//! [`FEATURE_FOCUS`](crate::remote::protocol::FEATURE_FOCUS) — the gate lives
//! in [`crate::remote::handler::handle`]. Every action calls the same shared
//! function the desktop command uses (`commands::focus*`), so both surfaces
//! validate and emit identically and the phone gets the desktop's JSON.
//!
//! AI requests can take minutes: they are acked as soon as they are accepted
//! and answer later with a `focus_ai_result`, unless the session is gone by
//! then. Failures only ever put a short code on the wire ([`error_code`]);
//! every outcome is audited exactly once.

use crate::commands::focus_ai::{self, FocusAiState};
use crate::focus::settings::{self as focus_settings, FocusSettings};
use crate::remote::audit;
use crate::remote::command::RejectReason;
use crate::remote::data::{self, optional_id, source_url, valid_id, MAX_TODO_TEXT};
use crate::remote::handler::{accept, command_ack, command_ack_for, reject, send_payload, HandlerCtx};
use crate::remote::protocol::{CmdPayload, StreamPayload};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Listener, Manager};

/// Most blocks a `focus_plan_to_calendar` may name (a day plan has a handful).
pub const MAX_BLOCK_IDS: usize = 100;
/// Longest error code put on the wire.
const MAX_CODE_LEN: usize = 40;
/// How long the live board waits for a burst of `focus:changed` to settle.
const LIVE_DEBOUNCE: Duration = Duration::from_millis(1_200);

// ---- Pure validation ---------------------------------------------------------

/// The short code a failure is reported with: the desktop error's prefix
/// before `:` when it is a `snake_case` word (`project_required`, `no_plan`,
/// `busy`, …), `cancelled` / `timeout`, else `failed`. The rest of the text
/// may quote user data or a provider's answer and never leaves the Host.
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

/// A local `YYYY-MM-DD` date, or none for blank.
pub fn valid_date(raw: Option<&str>) -> Result<Option<String>, RejectReason> {
    crate::focus::validate::due_date(raw).map_err(|_| RejectReason::Malformed)
}

/// The blocks a `focus_plan_to_calendar` names; absent means every block.
pub fn block_ids(raw: Option<&[String]>) -> Result<Option<Vec<String>>, RejectReason> {
    let Some(ids) = raw else { return Ok(None) };
    if ids.len() > MAX_BLOCK_IDS {
        return Err(RejectReason::Malformed);
    }
    ids.iter().map(|id| valid_id(Some(id))).collect::<Result<_, _>>().map(Some)
}

/// The id a phone's AI request runs under in [`FocusAiState`]: namespaced,
/// so a phone can only ever cancel its own requests.
fn engine_request_id(request_id: &str) -> String {
    format!("remote:{request_id}")
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

/// Which AI request a `focus_ai_*` action is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiKind {
    Suggest,
    Analyze,
    PlanToday,
}

impl AiKind {
    pub fn of_action(action: &str) -> Option<Self> {
        match action {
            "focus_ai_suggest" => Some(AiKind::Suggest),
            "focus_ai_analyze" => Some(AiKind::Analyze),
            "focus_ai_plan_today" => Some(AiKind::PlanToday),
            _ => None,
        }
    }

    /// The `ai_kind` of its `focus_ai_result`.
    pub fn as_str(self) -> &'static str {
        match self {
            AiKind::Suggest => "suggest",
            AiKind::Analyze => "analyze",
            AiKind::PlanToday => "plan_today",
        }
    }
}

// ---- Live board ----------------------------------------------------------------

/// The `focus:changed` listener of one session and the task that turns it into
/// pushes. Dropped with the session's [`HandlerCtx`].
pub struct LiveBoard {
    app: AppHandle,
    listener: tauri::EventId,
    task: tokio::task::AbortHandle,
}

impl Drop for LiveBoard {
    fn drop(&mut self) {
        self.app.unlisten(self.listener);
        self.task.abort();
    }
}

/// Push a fresh `todo_list` + `focus_board` to a `data` + `focus` session
/// whenever a todo changes anywhere (desktop, MCP, sync) or the Calendar
/// loads, coalescing bursts. The task only holds a weak reference: the
/// listener goes away with the session.
pub fn start_live_board(ctx: &Arc<HandlerCtx>) {
    if !(ctx.data && ctx.focus) || ctx.ipc {
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
    let _ = ctx.live_board.set(LiveBoard {
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

/// What an accepted action still owes the Controller after its ack.
#[derive(Default)]
struct Outcome {
    /// `todo_start_ai`: the new run (echoed in the ack).
    run_id: Option<String>,
    /// Follow the ack with a fresh `todo_list` + board.
    push_todos: bool,
}

type FocusResult = Result<Outcome, Failure>;

/// Run one Focus action (already allow-listed, rate-limited and flag-gated).
pub async fn handle(ctx: &Arc<HandlerCtx>, cmd: &CmdPayload) {
    let action = cmd.action.as_str();
    if action == "focus_plan_to_calendar" {
        // Acks from its own task.
        return plan_to_calendar(ctx, cmd).await;
    }
    let result = match action {
        "focus_board" => focus_board(ctx).await,
        "focus_ai_suggest" | "focus_ai_analyze" | "focus_ai_plan_today" => ai_request(ctx, cmd).await,
        "focus_ai_cancel" => ai_cancel(ctx, cmd).await,
        "focus_day_plan_get" => day_plan_get(ctx, cmd).await,
        "todo_start_ai" => start_ai(ctx, cmd).await,
        "focus_suggest_actual" => suggest_actual(ctx, cmd).await,
        "focus_settings_get" => settings_get(ctx).await,
        "focus_settings_set" => settings_set(ctx, cmd).await,
        "focus_calendar_refresh" => calendar_refresh(ctx).await,
        _ => Err(Failure::Rejected(RejectReason::NotAllowed)),
    };
    match result {
        Ok(outcome) => {
            accept(ctx, action, outcome.run_id.as_deref(), None).await;
            command_ack_for(ctx, cmd, true, None, outcome.run_id).await;
            if outcome.push_todos {
                data::push_todos(ctx).await;
            }
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

async fn require_todo(ctx: &HandlerCtx, cmd: &CmdPayload) -> Result<String, Failure> {
    let id = valid_id(cmd.item_id.as_deref())?;
    if !data::exists(&ctx.db, "todos", &id).await {
        return Err(RejectReason::NotFound.into());
    }
    Ok(id)
}

fn ai_state(ctx: &HandlerCtx) -> Result<FocusAiState, Failure> {
    ctx.app
        .try_state::<FocusAiState>()
        .map(|s| s.inner().clone())
        .ok_or_else(|| failed("no Focus AI state".into()))
}

async fn focus_board(ctx: &HandlerCtx) -> FocusResult {
    data::push_focus_board(ctx).await.map_err(failed)?;
    Ok(Outcome::default())
}

/// The validated input of one AI request.
enum AiInput {
    Suggest {
        source_url: String,
        project_id: Option<String>,
        title: Option<String>,
    },
    Analyze {
        todo_id: String,
    },
    PlanToday,
}

/// Validate, spawn and ack; the answer follows as a `focus_ai_result`.
async fn ai_request(ctx: &Arc<HandlerCtx>, cmd: &CmdPayload) -> FocusResult {
    let kind = AiKind::of_action(&cmd.action).ok_or(RejectReason::NotAllowed)?;
    let request_id = valid_id(cmd.request_id.as_deref())?;
    let input = match kind {
        AiKind::Suggest => {
            let source_url = source_url(cmd.source_url.as_deref())?.ok_or(RejectReason::Malformed)?;
            let project_id = optional_id(cmd.project_id.as_deref())?;
            if let Some(pid) = project_id.as_deref() {
                if !data::exists(&ctx.db, "projects", pid).await {
                    return Err(RejectReason::NotFound.into());
                }
            }
            let title = cmd.title.as_deref().map(str::trim).filter(|t| !t.is_empty());
            if title.is_some_and(|t| t.chars().count() > MAX_TODO_TEXT) {
                return Err(RejectReason::Malformed.into());
            }
            AiInput::Suggest {
                source_url,
                project_id,
                title: title.map(str::to_string),
            }
        }
        AiKind::Analyze => AiInput::Analyze {
            todo_id: require_todo(ctx, cmd).await?,
        },
        AiKind::PlanToday => AiInput::PlanToday,
    };
    let state = ai_state(ctx)?;
    let weak = Arc::downgrade(ctx);
    let (app, db) = (ctx.app.clone(), ctx.db.clone());
    tokio::spawn(async move {
        let out = run_ai(&app, &db, &state, input, engine_request_id(&request_id)).await;
        // The session ended meanwhile: nobody to tell.
        let Some(ctx) = weak.upgrade() else { return };
        deliver_ai(&ctx, kind, request_id, out).await;
    });
    Ok(Outcome::default())
}

/// What an AI request produced, already in its wire shape.
struct AiOutput {
    result: Value,
    /// `plan_today`: the plan's date (a `focus_day_plan` follows).
    plan_date: Option<String>,
}

async fn run_ai(
    app: &AppHandle,
    db: &crate::db::Db,
    state: &FocusAiState,
    input: AiInput,
    request_id: String,
) -> Result<AiOutput, String> {
    let rid = Some(request_id);
    let to_value = |v: Result<Value, serde_json::Error>| v.map_err(|e| e.to_string());
    match input {
        AiInput::Suggest {
            source_url,
            project_id,
            title,
        } => {
            let s = focus_ai::suggest_inner(app, db, state, source_url, project_id, title, rid).await?;
            Ok(AiOutput {
                result: to_value(serde_json::to_value(s))?,
                plan_date: None,
            })
        }
        AiInput::Analyze { todo_id } => {
            let r = focus_ai::analyze_inner(app, db, state, todo_id, rid).await?;
            Ok(AiOutput {
                result: to_value(serde_json::to_value(r))?,
                plan_date: None,
            })
        }
        AiInput::PlanToday => {
            let plan = focus_ai::plan_today_inner(app, db, state, rid).await?;
            Ok(AiOutput {
                plan_date: Some(plan.date.clone()),
                result: to_value(serde_json::to_value(plan))?,
            })
        }
    }
}

async fn deliver_ai(ctx: &HandlerCtx, kind: AiKind, request_id: String, out: Result<AiOutput, String>) {
    let out = match out {
        Ok(out) => out,
        Err(e) => {
            tracing::debug!(event = "remote_focus_ai_failed", ai_kind = kind.as_str(), error = %e);
            let payload = StreamPayload::FocusAiResult {
                request_id,
                ai_kind: kind.as_str().into(),
                ok: false,
                result: None,
                error: Some(error_code(&e).to_string()),
            };
            send_payload(ctx, payload).await;
            return;
        }
    };
    let plan = out.plan_date.map(|date| (date, out.result.clone()));
    let payload = StreamPayload::FocusAiResult {
        request_id,
        ai_kind: kind.as_str().into(),
        ok: true,
        result: Some(out.result),
        error: None,
    };
    send_payload(ctx, payload).await;
    match kind {
        // The todo's `ai_plan` changed.
        AiKind::Analyze => data::push_todos(ctx).await,
        AiKind::PlanToday => {
            if let Some((date, plan)) = plan {
                send_payload(ctx, StreamPayload::FocusDayPlan { date, plan: Some(plan) }).await;
            }
        }
        AiKind::Suggest => {}
    }
}

/// Accepted even when nothing runs under that id (any more).
async fn ai_cancel(ctx: &HandlerCtx, cmd: &CmdPayload) -> FocusResult {
    let request_id = valid_id(cmd.request_id.as_deref())?;
    let state = ai_state(ctx)?;
    state.cancel(&engine_request_id(&request_id)).await;
    Ok(Outcome::default())
}

async fn send_day_plan(ctx: &HandlerCtx, date: Option<String>) -> Result<(), String> {
    let (date, plan) = focus_ai::day_plan_get_inner(&ctx.db, date).await?;
    let plan = plan.map(serde_json::to_value).transpose().map_err(|e| e.to_string())?;
    send_payload(ctx, StreamPayload::FocusDayPlan { date, plan }).await;
    Ok(())
}

async fn day_plan_get(ctx: &HandlerCtx, cmd: &CmdPayload) -> FocusResult {
    let date = valid_date(cmd.date.as_deref())?;
    send_day_plan(ctx, date).await.map_err(failed)?;
    Ok(Outcome::default())
}

/// One Calendar call per block: run in a task so the session's other commands
/// (permission answers…) are not held behind it. The stored plan is pushed
/// either way — a partial failure still saved the events it created — then
/// the ack, so the phone sees the result before its write settles.
async fn plan_to_calendar(ctx: &Arc<HandlerCtx>, cmd: &CmdPayload) {
    let action = cmd.action.as_str();
    let input = valid_date(cmd.date.as_deref())
        .and_then(|d| d.ok_or(RejectReason::Malformed))
        .and_then(|d| Ok((d, block_ids(cmd.block_ids.as_deref())?)));
    let (date, blocks) = match input {
        Ok(v) => v,
        Err(reason) => {
            reject(ctx, action, None, reason).await;
            command_ack(ctx, cmd, false, Some(reason.as_str())).await;
            return;
        }
    };
    let weak = Arc::downgrade(ctx);
    let db = ctx.db.clone();
    let cmd = cmd.clone();
    tokio::spawn(async move {
        let out = focus_ai::plan_to_calendar_inner(&db, date.clone(), blocks).await;
        let Some(ctx) = weak.upgrade() else { return };
        if let Err(e) = send_day_plan(&ctx, Some(date)).await {
            tracing::debug!(event = "remote_focus_day_plan_failed", error = %e);
        }
        let action = cmd.action.as_str();
        match out {
            Ok(_) => {
                accept(&ctx, action, None, None).await;
                command_ack(&ctx, &cmd, true, None).await;
            }
            Err(e) => {
                let Failure::Failed(code) = failed(e) else { return };
                audit::audit(&ctx.db, None, Some(&ctx.room_id), None, action, audit::RESULT_REJECTED, Some(&code))
                    .await;
                command_ack(&ctx, &cmd, false, Some(&code)).await;
            }
        }
    });
}

/// A session working on the todo, streamed to this Controller from its first
/// event (like a `start_run` that created one).
async fn start_ai(ctx: &HandlerCtx, cmd: &CmdPayload) -> FocusResult {
    // Like a `start_run` that creates a session: only a Controller that can
    // navigate to a run other than its linked one.
    if !ctx.multi_run {
        return Err(RejectReason::NotAllowed.into());
    }
    let todo_id = require_todo(ctx, cmd).await?;
    let run_id = focus_ai::start_with_ai_inner(&ctx.app, &ctx.db, &ctx.registry, todo_id)
        .await
        .map_err(failed)?;
    ctx.subscriptions.lock().await.insert(run_id.clone());
    *ctx.focus_run_id.lock().await = Some(run_id.clone());
    Ok(Outcome {
        run_id: Some(run_id),
        push_todos: true,
    })
}

async fn suggest_actual(ctx: &HandlerCtx, cmd: &CmdPayload) -> FocusResult {
    let todo_id = require_todo(ctx, cmd).await?;
    let minutes = focus_ai::suggest_actual_inner(&ctx.db, &todo_id).await.map_err(failed)?;
    send_payload(ctx, StreamPayload::FocusActual { todo_id, minutes }).await;
    Ok(Outcome::default())
}

async fn send_settings(ctx: &HandlerCtx, settings: &FocusSettings) -> Result<(), Failure> {
    let settings = serde_json::to_value(settings).map_err(|e| failed(e.to_string()))?;
    send_payload(ctx, StreamPayload::FocusSettings { settings }).await;
    Ok(())
}

async fn settings_get(ctx: &HandlerCtx) -> FocusResult {
    let settings = focus_settings::load(&ctx.db).await.map_err(failed)?;
    send_settings(ctx, &settings).await?;
    Ok(Outcome::default())
}

/// Save through the desktop's validation, then push the stored settings and
/// the board they re-rank.
async fn settings_set(ctx: &HandlerCtx, cmd: &CmdPayload) -> FocusResult {
    let current = focus_settings::load(&ctx.db).await.map_err(failed)?;
    let merged = merge_settings(&current, cmd.settings.as_ref())?;
    let valid = merged.validated().map_err(|e| {
        tracing::debug!(event = "remote_focus_settings_invalid", error = %e);
        Failure::Failed("invalid".into())
    })?;
    let saved = focus_settings::save(&ctx.db, valid).await.map_err(failed)?;
    crate::focus::emit_todos_changed(&ctx.app);
    send_settings(ctx, &saved).await?;
    if let Err(e) = data::push_focus_board(ctx).await {
        tracing::debug!(event = "remote_focus_board_failed", error = %e);
    }
    Ok(Outcome::default())
}

/// Drop the cached Calendar events (desktop's refresh button), then the board
/// with a fresh load.
async fn calendar_refresh(ctx: &HandlerCtx) -> FocusResult {
    crate::commands::focus::focus_refresh_calendar(ctx.app.clone())
        .await
        .map_err(failed)?;
    data::push_focus_board(ctx).await.map_err(failed)?;
    Ok(Outcome::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn errors_reach_the_wire_as_a_code_only() {
        for (e, code) in [
            ("project_required: pick a project for this todo first", "project_required"),
            ("no_plan: no plan for this day", "no_plan"),
            ("busy: an AI request for this is already running", "busy"),
            ("calendar_failed: Create event failed: 403 token ya29.SECRET", "calendar_failed"),
            ("ai_invalid_answer: missing field", "ai_invalid_answer"),
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
    fn dates_and_block_ids() {
        assert_eq!(valid_date(Some("2026-11-03")), Ok(Some("2026-11-03".into())));
        assert_eq!(valid_date(Some(" ")), Ok(None));
        assert_eq!(valid_date(None), Ok(None));
        for bad in ["03/11/2026", "2026-13-01", "today", "2026-11-03T00:00:00Z"] {
            assert_eq!(valid_date(Some(bad)), Err(RejectReason::Malformed), "{bad}");
        }

        assert_eq!(block_ids(None), Ok(None));
        let ids = vec![" b1 ".to_string(), "b2".to_string()];
        assert_eq!(block_ids(Some(&ids)), Ok(Some(vec!["b1".into(), "b2".into()])));
        assert_eq!(block_ids(Some(&[])), Ok(Some(vec![])));
        for bad in [vec!["".to_string()], vec!["a\nb".to_string()], vec!["x".repeat(129)]] {
            assert_eq!(block_ids(Some(&bad)), Err(RejectReason::Malformed), "{bad:?}");
        }
        let many: Vec<String> = (0..=MAX_BLOCK_IDS).map(|i| format!("b{i}")).collect();
        assert_eq!(block_ids(Some(&many)), Err(RejectReason::Malformed));
    }

    #[test]
    fn request_ids_are_namespaced_for_the_engine() {
        assert_eq!(engine_request_id("r1"), "remote:r1");
        // The wire id itself goes through valid_id (≤ 128, printable).
        assert_eq!(valid_id(Some(" r1 ")), Ok("r1".into()));
        assert_eq!(valid_id(Some(&"r".repeat(129))), Err(RejectReason::Malformed));
        assert_eq!(valid_id(None), Err(RejectReason::Malformed));
    }

    #[test]
    fn ai_kinds() {
        assert_eq!(AiKind::of_action("focus_ai_suggest"), Some(AiKind::Suggest));
        assert_eq!(AiKind::of_action("focus_ai_analyze"), Some(AiKind::Analyze));
        assert_eq!(AiKind::of_action("focus_ai_plan_today"), Some(AiKind::PlanToday));
        assert_eq!(AiKind::of_action("focus_ai_cancel"), None);
        assert_eq!(
            [AiKind::Suggest, AiKind::Analyze, AiKind::PlanToday].map(AiKind::as_str),
            ["suggest", "analyze", "plan_today"]
        );
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
        let v = serde_json::to_value(StreamPayload::FocusAiResult {
            request_id: "r1".into(),
            ai_kind: "suggest".into(),
            ok: true,
            result: Some(json!({ "title": "x" })),
            error: None,
        })
        .unwrap();
        assert_eq!(
            v,
            json!({ "kind": "focus_ai_result", "request_id": "r1", "ai_kind": "suggest", "ok": true, "result": { "title": "x" } })
        );
        let v = serde_json::to_value(StreamPayload::FocusAiResult {
            request_id: "r2".into(),
            ai_kind: "analyze".into(),
            ok: false,
            result: None,
            error: Some("busy".into()),
        })
        .unwrap();
        assert_eq!(
            v,
            json!({ "kind": "focus_ai_result", "request_id": "r2", "ai_kind": "analyze", "ok": false, "error": "busy" })
        );
        let v = serde_json::to_value(StreamPayload::FocusDayPlan { date: "2026-11-03".into(), plan: None }).unwrap();
        assert_eq!(v, json!({ "kind": "focus_day_plan", "date": "2026-11-03", "plan": null }));
        let v = serde_json::to_value(StreamPayload::FocusActual { todo_id: "t1".into(), minutes: None }).unwrap();
        assert_eq!(v, json!({ "kind": "focus_actual", "todo_id": "t1", "minutes": null }));
        let v = serde_json::to_value(StreamPayload::FocusActual { todo_id: "t1".into(), minutes: Some(95) }).unwrap();
        assert_eq!(v["minutes"], 95);
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
            "action": "focus_plan_to_calendar",
            "date": "2026-11-03",
            "block_ids": ["b1"],
            "source_url": "o/r#1",
            "settings": { "work_start": "08:00" },
        }))
        .unwrap();
        assert_eq!(cmd.date.as_deref(), Some("2026-11-03"));
        assert_eq!(cmd.block_ids, Some(vec!["b1".to_string()]));
        assert_eq!(cmd.source_url.as_deref(), Some("o/r#1"));
        assert_eq!(cmd.settings, Some(json!({ "work_start": "08:00" })));
        let old: CmdPayload = serde_json::from_value(json!({ "action": "todo_list" })).unwrap();
        assert!(old.date.is_none() && old.block_ids.is_none() && old.settings.is_none());
    }
}
