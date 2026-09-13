//! Command orchestration: turn a decrypted Controller command into a call to
//! the existing internal run logic, enforcing every security rule and auditing
//! the outcome (FR-007/008/009, BR-008/009/011/016/017).
//!
//! The PURE rules live in [`crate::remote::command`] (allow-list, decision map,
//! idempotency, rate limit) and are unit-tested there. This module wires them
//! to real I/O:
//!   - allow-list gate (AC-12)
//!   - rate-limit 30/min (AC-18)
//!   - `respond_permission`: once-only decisions + idempotency (AC-10/AC-17)
//!   - `start_run`: permission mode FORCED to default (AC-11)
//!   - `cancel_run`, `send_chat_message`, `request_history`
//!
//! Every path (accepted or rejected) writes exactly one audit row (AC-14).

use crate::db::Db;
use crate::remote::agent::{now_unix, IdleClock, IDLE_TTL_SECS};
use crate::remote::audit;
use crate::remote::command::{
    is_allowed_action, map_remote_decision, CommandRateLimiter, IdempotencyGuard, RejectReason,
};
use crate::remote::forwarder::{
    replay_history, run_log_path, send_engine_model_options, send_plan_usage,
    send_project_file_list, send_project_list, send_run_list, send_slash_command_list, SeqCounter,
};
use crate::remote::outbound::OutboundTx;
use crate::remote::{FocusRun, RunSubscriptions};
use crate::remote::protocol::{CmdPayload, Envelope, FrameType, ImageAttachmentWire, StreamPayload};
use crate::runs::{BrokerApprovals, RunRegistry};
use remote_e2e::Session;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex as TokioMutex;

/// Payload for the `run:permission_resolved:<run_id>` Tauri event (FIX-B). Lets
/// the desktop UI drop a prompt from its queue when a remote Controller answered.
#[derive(Debug, Clone, Serialize)]
struct PermissionResolved {
    request_id: String,
}

/// Everything a handler needs to execute a command and reply on the room.
pub struct HandlerCtx {
    pub app: AppHandle,
    pub db: Db,
    pub registry: RunRegistry,
    pub broker_approvals: BrokerApprovals,
    pub session: Arc<Session>,
    pub room_id: String,
    /// The run the session link was minted for. Always subscribed, and the only
    /// run a legacy (non-`multi_run`) Controller may touch.
    pub initial_run_id: String,
    /// Whether the Controller can drive runs beyond `initial_run_id`
    /// ([`FEATURE_MULTI_RUN`]).
    pub multi_run: bool,
    /// Runs whose live transcript is being streamed. Shared with the forwarder.
    pub subscriptions: RunSubscriptions,
    /// The run the Controller is currently viewing; surfaced to the desktop UI
    /// so it can show WHERE the phone is.
    pub focus_run_id: FocusRun,
    /// Outbound channel to the relay (for notices / history replay).
    pub out: OutboundTx,
    pub seq: Arc<SeqCounter>,
    /// Per-Controller state, shared across commands within a room.
    pub rate: Arc<TokioMutex<CommandRateLimiter>>,
    pub idem: Arc<TokioMutex<IdempotencyGuard>>,
    /// Sliding idle deadline (Unix secs). Bumped on each accepted command.
    pub idle: IdleClock,
}

impl HandlerCtx {
    /// Slide the idle window forward (only on an accepted inbound command, never
    /// on forwarded output). Also refreshes the persisted session credential so
    /// a reconnect within 3h skips the OTP.
    fn slide_idle(&self) {
        let deadline = now_unix() + IDLE_TTL_SECS;
        self.idle.store(deadline, Ordering::SeqCst);
        if let Some(mut cred) = crate::secrets::get_remote_session() {
            // The credential is per SESSION, keyed by the link's run.
            if cred.run_id == self.initial_run_id {
                cred.idle_expires_at = deadline;
                let _ = crate::secrets::set_remote_session(&cred);
            }
        }
    }

    /// `None` for a multi-run Controller (list every run), otherwise the single
    /// run a legacy Controller is allowed to see.
    fn list_filter(&self) -> Option<&str> {
        if self.multi_run {
            None
        } else {
            Some(&self.initial_run_id)
        }
    }

    /// The run a run-scoped read applies to: what the command named, else the
    /// run the Controller is currently viewing, else the linked run.
    async fn target_run(&self, cmd: &CmdPayload) -> String {
        if let Some(rid) = cmd.run_id.clone().filter(|r| !r.trim().is_empty()) {
            return rid;
        }
        if let Some(focus) = self.focus_run_id.lock().await.clone() {
            return focus;
        }
        self.initial_run_id.clone()
    }

    /// Whether this session may act on `run_id` at all.
    async fn may_touch(&self, run_id: &str) -> bool {
        if self.multi_run {
            // Device approval is the trust boundary (SRS BR-005 v1.1): any run
            // the Host actually has is fair game.
            return run_exists(&self.db, run_id).await;
        }
        run_id == self.initial_run_id
    }
}

/// Locate a run's persisted log via its project path (INT-004). `None` when the
/// run (or its project) is unknown.
async fn resolve_run_log(db: &Db, run_id: &str) -> Option<std::path::PathBuf> {
    let project_path: Option<String> = sqlx::query_scalar(
        "SELECT p.path FROM runs r JOIN projects p ON p.id = r.project_id WHERE r.id = ?",
    )
    .bind(run_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    Some(run_log_path(&project_path?, run_id))
}

/// Whether `run_id` names a real run on this Host.
async fn run_exists(db: &Db, run_id: &str) -> bool {
    sqlx::query("SELECT 1 FROM runs WHERE id = ? LIMIT 1")
        .bind(run_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .is_some()
}

/// Send a plaintext-kind notice (sealed) back to the Controller.
async fn notice(ctx: &HandlerCtx, run_id: Option<String>, text: &str) {
    let payload = StreamPayload::Notice {
        run_id,
        text: text.to_string(),
    };
    if let Ok(json) = serde_json::to_vec(&payload) {
        let cipher = ctx.session.seal(&json);
        let env = Envelope::data(FrameType::Stream, &ctx.room_id, cipher, ctx.seq.next());
        let _ = ctx.out.send(env);
    }
}

async fn send_payload(ctx: &HandlerCtx, payload: StreamPayload) {
    if let Ok(json) = serde_json::to_vec(&payload) {
        let cipher = ctx.session.seal(&json);
        let env = Envelope::data(FrameType::Stream, &ctx.room_id, cipher, ctx.seq.next());
        let _ = ctx.out.send(env);
    }
}

async fn command_ack(
    ctx: &HandlerCtx,
    cmd: &CmdPayload,
    accepted: bool,
    reason: Option<&str>,
) {
    let Some(command_id) = cmd.command_id.clone() else {
        return;
    };
    send_payload(
        ctx,
        StreamPayload::CommandAck {
            command_id,
            action: cmd.action.clone(),
            accepted,
            reason: reason.map(str::to_string),
        },
    )
    .await;
}

async fn permission_resolved(
    ctx: &HandlerCtx,
    run_id: String,
    request_id: String,
    accepted: bool,
    reason: Option<&str>,
) {
    send_payload(
        ctx,
        StreamPayload::PermissionResolved {
            run_id,
            request_id,
            accepted,
            reason: reason.map(str::to_string),
        },
    )
    .await;
}

/// Audit a rejection with a machine-readable reason (AC-14) and notify the
/// Controller so it can surface the refusal.
async fn reject(ctx: &HandlerCtx, action: &str, run_id: Option<&str>, reason: RejectReason) {
    audit::audit(
        &ctx.db,
        None,
        Some(&ctx.room_id),
        run_id,
        action,
        audit::RESULT_REJECTED,
        Some(reason.as_str()),
    )
    .await;
    notice(
        ctx,
        run_id.map(str::to_string),
        &format!("command rejected: {}", reason.as_str()),
    )
    .await;
}

/// Handle one decrypted Controller command end to end.
///
/// Order of checks is security-first: authenticated (implicit — the OTP gate
/// built this ctx) → wrong-run → rate limit → allow-list → per-action
/// validation → execute. A failure at any gate audits and returns without
/// touching run state. Reaching this function means the session is authenticated
/// (the OTP gate is what mints the [`HandlerCtx`]).
pub async fn handle(ctx: &HandlerCtx, cmd: CmdPayload) {
    let action = cmd.action.as_str();
    let run_id = cmd.run_id.clone();

    // Gate 0: run scope. A multi-run Controller may act on any run this Host
    // actually has (device approval is the trust boundary, SRS BR-005 v1.1); a
    // legacy one is still pinned to the run its link was minted for. Either way
    // a run_id the Host does not know is refused rather than passed downstream.
    if let Some(rid) = run_id.as_deref() {
        if !ctx.may_touch(rid).await {
            reject(ctx, action, run_id.as_deref(), RejectReason::WrongRun).await;
            return;
        }
    }

    // Gate 1: rate limit per Controller (BR-017/AC-18). Mutating commands keep
    // the 30/min cap; read-only browsing draws on its own budget so navigating
    // the run list can never starve the budget that guards execution.
    {
        let mut rl = ctx.rate.lock().await;
        if !rl.allow(action, Instant::now()) {
            drop(rl);
            reject(ctx, action, run_id.as_deref(), RejectReason::RateLimited).await;
            return;
        }
    }

    // Gate 2: allow-list (FR-009/AC-12). Anything else → reject + audit.
    if !is_allowed_action(action) {
        reject(ctx, action, run_id.as_deref(), RejectReason::NotAllowed).await;
        return;
    }

    // An accepted inbound command slides the idle window forward (never output).
    ctx.slide_idle();

    match action {
        "respond_permission" => handle_respond_permission(ctx, &cmd).await,
        "start_run" => handle_start_run(ctx, &cmd).await,
        "cancel_run" => handle_cancel_run(ctx, &cmd).await,
        "send_chat_message" => handle_send_chat(ctx, &cmd).await,
        "set_run_meta" => handle_set_run_meta(ctx, &cmd).await,
        "request_history" => handle_request_history(ctx, &cmd).await,
        "list_runs" => handle_list_runs(ctx).await,
        "list_projects" => handle_list_projects(ctx).await,
        "list_slash_commands" => handle_list_slash_commands(ctx).await,
        "list_engine_models" => handle_list_engine_models(ctx).await,
        "list_plan_usage" => handle_list_plan_usage(ctx, &cmd).await,
        "list_project_files" => handle_list_project_files(ctx, &cmd).await,
        "open_run" => handle_open_run(ctx, &cmd).await,
        "close_run" => handle_close_run(ctx, &cmd).await,
        // Unreachable: allow-list already gated. Defensive audit.
        _ => reject(ctx, action, run_id.as_deref(), RejectReason::NotAllowed).await,
    }
}

/// Record an accepted command (AC-14).
async fn accept(ctx: &HandlerCtx, action: &str, run_id: Option<&str>, reason: Option<&str>) {
    audit::audit(
        &ctx.db,
        None,
        Some(&ctx.room_id),
        run_id,
        action,
        audit::RESULT_ACCEPTED,
        reason,
    )
    .await;
}

/// Convert wire image attachments (base64) into the internal [`ImageAttachment`].
fn convert_attachments(
    attachments: &Option<Vec<ImageAttachmentWire>>,
) -> Vec<crate::commands::runs::ImageAttachment> {
    attachments
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|a| crate::commands::runs::ImageAttachment {
            media_type: a.mime.clone(),
            data: a.data_base64.clone(),
        })
        .collect()
}

/// Append @mention file paths to the prompt text as backticked tokens (simple
/// inlining — the model sees the referenced paths). Returns the combined text.
fn inline_mentions(text: Option<&str>, mentions: &Option<Vec<String>>) -> Option<String> {
    let base = text.unwrap_or("").to_string();
    let Some(paths) = mentions.as_ref().filter(|p| !p.is_empty()) else {
        return text.map(str::to_string);
    };
    let refs = paths
        .iter()
        .map(|p| format!("`{p}`"))
        .collect::<Vec<_>>()
        .join(" ");
    let combined = if base.trim().is_empty() {
        format!("Referenced files: {refs}")
    } else {
        format!("{base}\n\nReferenced files: {refs}")
    };
    Some(combined)
}

async fn handle_respond_permission(ctx: &HandlerCtx, cmd: &CmdPayload) {
    let action = "respond_permission";
    let (Some(run_id), Some(request_id)) = (cmd.run_id.clone(), cmd.request_id.clone()) else {
        reject(ctx, action, cmd.run_id.as_deref(), RejectReason::Malformed).await;
        return;
    };
    let decision = cmd.decision.as_deref().unwrap_or("");

    // BR-016/SEC-011/AC-17: only allow_once / deny_once; reject remember +
    // anything else, and map to the internal allow/deny.
    let internal = match map_remote_decision(decision) {
        Ok(d) => d,
        Err(reason) => {
            reject(ctx, action, Some(&run_id), reason).await;
            return;
        }
    };

    // BR-009/AC-10: idempotent by request_id. A duplicate (already answered by
    // the Owner at the Host, or a re-sent frame) is dropped, not re-applied.
    {
        let mut idem = ctx.idem.lock().await;
        if !idem.claim(&request_id) {
            drop(idem);
            audit::audit(
                &ctx.db,
                None,
                Some(&ctx.room_id),
                Some(&run_id),
                action,
                audit::RESULT_REJECTED,
                Some(RejectReason::Duplicate.as_str()),
            )
            .await;
            permission_resolved(
                ctx,
                run_id.clone(),
                request_id,
                false,
                Some("already_handled"),
            )
            .await;
            notice(ctx, Some(run_id), "already handled").await;
            return;
        }
    }

    let payload = crate::commands::runs::RespondPermissionPayload {
        run_id: run_id.clone(),
        request_id: request_id.clone(),
        decision: internal.to_string(),
        reason: None,
        // AskUserQuestion answers ride alongside an allow_once decision; the
        // decision gate above already enforced once-only (BR-016/AC-17), so
        // forwarding answers never relaxes it.
        answers: cmd.answers.clone(),
        response: None,
    };
    match crate::commands::runs::respond_permission_inner(
        &ctx.registry,
        &ctx.broker_approvals,
        payload,
    )
    .await
    {
        Ok(()) => {
            accept(ctx, action, Some(&run_id), None).await;
            permission_resolved(
                ctx,
                run_id.clone(),
                request_id.clone(),
                true,
                None,
            )
            .await;
            // Tell the local desktop UI the request is resolved so it drops the
            // still-pending prompt from its permissionQueue (FIX-B). Emitted only
            // on success; the frontend filters by request_id so it is idempotent.
            let event = format!("run:permission_resolved:{run_id}");
            let _ = ctx.app.emit(
                &event,
                PermissionResolved {
                    request_id: request_id.clone(),
                },
            );
        }
        Err(e) => {
            // Delivery did not reach a resolver, so this request has not been
            // handled. Roll the idempotency claim back and allow a retry.
            ctx.idem.lock().await.release(&request_id);
            // Execution failed (run not active etc). Audit as rejected with a
            // generic reason — NEVER echo secret/plaintext into the reason.
            tracing::debug!(event = "remote_respond_permission_failed", error = %e);
            reject(ctx, action, Some(&run_id), RejectReason::Malformed).await;
            permission_resolved(
                ctx,
                run_id,
                request_id,
                false,
                Some("delivery_failed"),
            )
            .await;
        }
    }
}

async fn handle_start_run(ctx: &HandlerCtx, cmd: &CmdPayload) {
    use sqlx::Row;
    use tauri::Manager;
    let action = "start_run";

    // Resolve the run to start. Two shapes: an existing run_id, or a
    // project_id+text (create a session run first, mirroring the local UI). Only
    // the single bound run may be started (wrong-run gate already covers a
    // mismatching run_id; here we only accept the bound run or a fresh session).
    let run_id = match (cmd.run_id.clone(), cmd.project_id.clone()) {
        (Some(run_id), _) => run_id,
        (None, Some(project_id)) => {
            match create_session_run_for_remote(&ctx.db, &project_id, cmd.engine.clone()).await {
                Ok(id) => id,
                Err(e) => {
                    tracing::debug!(event = "remote_start_run_session_create_failed", error = %e);
                    reject(ctx, action, None, RejectReason::Malformed).await;
                    return;
                }
            }
        }
        (None, None) => {
            reject(ctx, action, None, RejectReason::Malformed).await;
            return;
        }
    };

    // Engine, model and permission mode hints from the controller are ALL
    // honored (desktop parity). An omitted/blank permission mode leaves the
    // override as None so `start_run_inner` falls back to the settings default.
    // Mirror the resolved selection into the shared meta store both ways.
    let permission_mode = cmd.permission_mode.clone().filter(|s| !s.trim().is_empty());
    let engine = cmd.engine.clone().filter(|s| !s.trim().is_empty());
    let model = cmd.model.clone().filter(|s| !s.trim().is_empty());
    publish_run_meta(ctx, &run_id, engine.clone(), model.clone(), permission_mode.clone());

    // Continue-vs-fresh decision — mirrors the desktop `submitFollowUp`: an
    // existing session that already captured a conversation (`session_id`) must be
    // RESUMED (`claude|codex --resume`) and then fed the message. Starting it fresh
    // spawns a NEW conversation that gets mirrored as a brand-new session on the
    // desktop (the reported bug). Only a fetched issue/PR first run, or a brand-new
    // session with no captured conversation yet, starts fresh.
    let meta =
        sqlx::query("SELECT status, type AS run_type, session_id, engine FROM runs WHERE id = ?")
            .bind(&run_id)
            .fetch_optional(&ctx.db)
            .await
            .ok()
            .flatten();
    let (status, _run_type, existing_session, run_engine) = match &meta {
        Some(r) => (
            r.get::<String, _>("status"),
            r.get::<String, _>("run_type"),
            r.get::<Option<String>, _>("session_id")
                .filter(|s| !s.trim().is_empty()),
            r.get::<String, _>("engine"),
        ),
        None => (String::new(), String::new(), None, String::new()),
    };
    let is_running = status == "running";
    // Resume any idle Claude/Codex run that ALREADY captured a session — do NOT
    // additionally require run_type == "session". The desktop follow-up
    // (submitFollowUp → resume_run) resumes purely by session_id + engine,
    // regardless of run type; gating the remote path on run_type made a conversed
    // issue/PR run (type "analyze_issue", etc.) fresh-start on every remote turn,
    // spawning a brand-new session (the reported bug). A run with no captured
    // conversation yet (a freshly fetched issue/PR) has existing_session=None, so
    // it still correctly starts fresh.
    let resumable = !is_running
        && existing_session.is_some()
        && (run_engine == "claude" || run_engine == "codex");

    if resumable || is_running {
        // A resume/follow-up needs message content; a fresh start can rely on the
        // cached issue/PR input instead.
        let content = inline_mentions(cmd.text.as_deref(), &cmd.mentions).unwrap_or_default();
        let images = convert_attachments(&cmd.attachments);
        if content.trim().is_empty() && images.is_empty() {
            reject(ctx, action, Some(&run_id), RejectReason::Malformed).await;
            return;
        }
        // Reattach the idle session first (desktop resume→send); a live run skips
        // straight to injecting the message.
        if resumable {
            let db_state = ctx.app.state::<crate::db::Db>();
            let reg_state = ctx.app.state::<crate::runs::RunRegistry>();
            if let Err(e) = crate::commands::runs::resume_run(
                ctx.app.clone(),
                db_state,
                reg_state,
                run_id.clone(),
                permission_mode.clone(),
                model.clone(),
                Some(false),
            )
            .await
            {
                tracing::debug!(event = "remote_resume_run_failed", error = %e);
                reject(ctx, action, Some(&run_id), RejectReason::Malformed).await;
                return;
            }
        }
        let payload = crate::commands::runs::SendUserMessagePayload {
            run_id: run_id.clone(),
            content,
            images,
            override_budget: false,
        };
        match crate::commands::runs::send_user_message_inner(&ctx.db, &ctx.registry, payload).await {
            Ok(()) => {
                accept(ctx, action, Some(&run_id), None).await;
                command_ack(ctx, cmd, true, None).await;
            }
            Err(e) => {
                tracing::debug!(event = "remote_start_run_send_failed", error = %e);
                reject(ctx, action, Some(&run_id), RejectReason::Malformed).await;
                command_ack(ctx, cmd, false, Some("malformed")).await;
            }
        }
        return;
    }

    // Fresh launch: fetched issue/PR first run, or a brand-new session.
    let payload = crate::commands::runs::StartRunPayload {
        run_id: run_id.clone(),
        engine_override: engine,
        permission_mode_override: permission_mode,
        prompt_override: inline_mentions(cmd.text.as_deref(), &cmd.mentions),
        model_override: model,
        images: convert_attachments(&cmd.attachments),
        override_budget: false,
    };
    match crate::commands::runs::start_run_inner(
        ctx.app.clone(),
        ctx.db.clone(),
        ctx.registry.clone(),
        payload,
    )
    .await
    {
        Ok(()) => {
            accept(ctx, action, Some(&run_id), None).await;
            command_ack(ctx, cmd, true, None).await;
        }
        Err(e) => {
            tracing::debug!(event = "remote_start_run_failed", error = %e);
            reject(ctx, action, Some(&run_id), RejectReason::Malformed).await;
            command_ack(ctx, cmd, false, Some("malformed")).await;
        }
    }
}

/// Fan a controller's engine/model/permission-mode change out to the shared meta
/// store → desktop composer + controller (via [`crate::remote::apply_run_meta`]).
fn publish_run_meta(
    ctx: &HandlerCtx,
    run_id: &str,
    engine: Option<String>,
    model: Option<String>,
    permission_mode: Option<String>,
) {
    use tauri::Manager;
    let (Some(store), Some(bus)) = (
        ctx.app.try_state::<crate::remote::RunMetaStore>(),
        ctx.app.try_state::<crate::remote::RemoteBus>(),
    ) else {
        return;
    };
    crate::remote::apply_run_meta(
        &ctx.app,
        &store,
        &bus,
        run_id,
        engine,
        model,
        permission_mode,
    );
}

/// Handle a controller `set_run_meta`: mirror its selector change onto the
/// desktop composer + the shared store (no run (re)start). The wrong-run gate in
/// [`handle`] already ensured `run_id` is the bound run.
async fn handle_set_run_meta(ctx: &HandlerCtx, cmd: &CmdPayload) {
    let action = "set_run_meta";
    let Some(run_id) = cmd.run_id.clone() else {
        reject(ctx, action, None, RejectReason::Malformed).await;
        return;
    };
    publish_run_meta(
        ctx,
        &run_id,
        cmd.engine.clone(),
        cmd.model.clone(),
        cmd.permission_mode.clone(),
    );
    accept(ctx, action, Some(&run_id), None).await;
}

async fn handle_cancel_run(ctx: &HandlerCtx, cmd: &CmdPayload) {
    let action = "cancel_run";
    let Some(run_id) = cmd.run_id.clone() else {
        reject(ctx, action, None, RejectReason::Malformed).await;
        return;
    };
    match crate::commands::runs::cancel_run_inner(&ctx.registry, &ctx.db, &run_id).await {
        Ok(()) => accept(ctx, action, Some(&run_id), None).await,
        Err(e) => {
            tracing::debug!(event = "remote_cancel_run_failed", error = %e);
            reject(ctx, action, Some(&run_id), RejectReason::Malformed).await;
        }
    }
}

async fn handle_send_chat(ctx: &HandlerCtx, cmd: &CmdPayload) {
    let action = "send_chat_message";
    let Some(run_id) = cmd.run_id.clone() else {
        reject(ctx, action, None, RejectReason::Malformed).await;
        return;
    };
    // A turn must carry text/mentions or at least one attachment.
    let content = inline_mentions(cmd.text.as_deref(), &cmd.mentions).unwrap_or_default();
    let images = convert_attachments(&cmd.attachments);
    if content.trim().is_empty() && images.is_empty() {
        reject(ctx, action, Some(&run_id), RejectReason::Malformed).await;
        return;
    }
    let payload = crate::commands::runs::SendUserMessagePayload {
        run_id: run_id.clone(),
        content,
        images,
        override_budget: false,
    };
    match crate::commands::runs::send_user_message_inner(&ctx.db, &ctx.registry, payload).await {
        Ok(()) => {
            accept(ctx, action, Some(&run_id), None).await;
            command_ack(ctx, cmd, true, None).await;
        }
        Err(e) => {
            tracing::debug!(event = "remote_send_chat_failed", error = %e);
            reject(ctx, action, Some(&run_id), RejectReason::Malformed).await;
            command_ack(ctx, cmd, false, Some("malformed")).await;
        }
    }
}

async fn handle_request_history(ctx: &HandlerCtx, cmd: &CmdPayload) {
    let action = "request_history";
    let Some(run_id) = cmd.run_id.clone() else {
        reject(ctx, action, None, RejectReason::Malformed).await;
        return;
    };
    let Some(path) = resolve_run_log(&ctx.db, &run_id).await else {
        reject(ctx, action, Some(&run_id), RejectReason::Malformed).await;
        return;
    };
    let sent = replay_history(
        &ctx.session,
        &ctx.room_id,
        &run_id,
        &path,
        &ctx.out,
        &ctx.seq,
    )
    .await;
    accept(ctx, action, Some(&run_id), Some(&format!("replayed={sent}"))).await;
}

/// Send the Controller the run list: every run for a multi-run Controller, the
/// single linked run for a legacy one.
async fn handle_list_runs(ctx: &HandlerCtx) {
    let action = "list_runs";
    send_run_list(
        &ctx.db,
        &ctx.session,
        &ctx.room_id,
        &ctx.out,
        &ctx.seq,
        ctx.list_filter(),
    )
    .await;
    accept(ctx, action, None, None).await;
}

/// Open a run: start streaming its transcript and push everything the composer
/// needs for it (history, meta, usage, project files) in one round trip.
///
/// This is the navigation primitive the run browser is built on. It only changes
/// what THIS session streams — never run state — which is why it draws on the
/// read-only rate budget.
async fn handle_open_run(ctx: &HandlerCtx, cmd: &CmdPayload) {
    let action = "open_run";
    let Some(run_id) = cmd.run_id.clone().filter(|r| !r.trim().is_empty()) else {
        reject(ctx, action, None, RejectReason::Malformed).await;
        return;
    };
    if !ctx.multi_run {
        // A legacy Controller renders one run; letting it switch would strand it.
        reject(ctx, action, Some(&run_id), RejectReason::NotAllowed).await;
        return;
    }
    // Gate 0 already proved the run exists and is in scope.
    ctx.subscriptions.lock().await.insert(run_id.clone());
    *ctx.focus_run_id.lock().await = Some(run_id.clone());
    accept(ctx, action, Some(&run_id), None).await;

    // Replay first so the live stream that follows lands on top of real context.
    if let Some(path) = resolve_run_log(&ctx.db, &run_id).await {
        replay_history(&ctx.session, &ctx.room_id, &run_id, &path, &ctx.out, &ctx.seq).await;
    }
    send_run_meta_for(ctx, &run_id).await;
    send_plan_usage(&ctx.db, &ctx.session, &ctx.room_id, &ctx.out, &ctx.seq, &run_id).await;
    send_project_file_list(
        &ctx.db,
        &ctx.session,
        &ctx.room_id,
        &ctx.out,
        &ctx.seq,
        &run_id,
    )
    .await;
}

/// Close a run: stop streaming its transcript to save the phone's bandwidth.
/// Permission requests and status changes still arrive (that is the point of the
/// run browser), so closing a run never hides something that needs an answer.
async fn handle_close_run(ctx: &HandlerCtx, cmd: &CmdPayload) {
    let action = "close_run";
    let Some(run_id) = cmd.run_id.clone().filter(|r| !r.trim().is_empty()) else {
        reject(ctx, action, None, RejectReason::Malformed).await;
        return;
    };
    if !ctx.multi_run {
        reject(ctx, action, Some(&run_id), RejectReason::NotAllowed).await;
        return;
    }
    ctx.subscriptions.lock().await.remove(&run_id);
    {
        let mut focus = ctx.focus_run_id.lock().await;
        if focus.as_deref() == Some(run_id.as_str()) {
            *focus = None;
        }
    }
    accept(ctx, action, Some(&run_id), None).await;
}

/// Push the engine/model/permission-mode snapshot for one run.
async fn send_run_meta_for(ctx: &HandlerCtx, run_id: &str) {
    use tauri::Manager;
    if let Some(store) = ctx.app.try_state::<crate::remote::RunMetaStore>() {
        crate::remote::forwarder::send_run_meta(
            &ctx.db,
            &store,
            &ctx.session,
            &ctx.room_id,
            &ctx.out,
            &ctx.seq,
            run_id,
        )
        .await;
    }
}

/// Send the Controller the list of projects (kept for parity).
async fn handle_list_projects(ctx: &HandlerCtx) {
    let action = "list_projects";
    send_project_list(&ctx.db, &ctx.session, &ctx.room_id, &ctx.out, &ctx.seq).await;
    accept(ctx, action, None, None).await;
}

/// Send the Controller the slash-command palette.
async fn handle_list_slash_commands(ctx: &HandlerCtx) {
    let action = "list_slash_commands";
    send_slash_command_list(&ctx.session, &ctx.room_id, &ctx.out, &ctx.seq).await;
    accept(ctx, action, None, None).await;
}

/// Send the Controller the engine/model selector options.
async fn handle_list_engine_models(ctx: &HandlerCtx) {
    let action = "list_engine_models";
    send_engine_model_options(&ctx.db, &ctx.session, &ctx.room_id, &ctx.out, &ctx.seq).await;
    accept(ctx, action, None, None).await;
}

/// Send the Controller the subscription plan-usage badges (Claude + Codex) for
/// the run it names, or the one it is currently viewing.
async fn handle_list_plan_usage(ctx: &HandlerCtx, cmd: &CmdPayload) {
    let action = "list_plan_usage";
    let run_id = ctx.target_run(cmd).await;
    send_plan_usage(&ctx.db, &ctx.session, &ctx.room_id, &ctx.out, &ctx.seq, &run_id).await;
    accept(ctx, action, Some(&run_id), None).await;
}

/// Send the Controller a bounded listing of project files for the run it names,
/// or the one it is currently viewing (for `@`-mention completion).
async fn handle_list_project_files(ctx: &HandlerCtx, cmd: &CmdPayload) {
    let action = "list_project_files";
    let run_id = ctx.target_run(cmd).await;
    send_project_file_list(
        &ctx.db,
        &ctx.session,
        &ctx.room_id,
        &ctx.out,
        &ctx.seq,
        &run_id,
    )
    .await;
    accept(ctx, action, Some(&run_id), None).await;
}

/// Create a session run remotely (mirrors `create_session_run`), using the given
/// engine hint or the project's default engine. Returns the new run id.
async fn create_session_run_for_remote(
    db: &Db,
    project_id: &str,
    engine_hint: Option<String>,
) -> Result<String, String> {
    let engine = match engine_hint.filter(|s| !s.trim().is_empty()) {
        Some(e) => e,
        None => crate::commands::settings::resolve_default_engine(db).await,
    };
    let new_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO runs (id, project_id, type, status, engine, created_at)
         VALUES (?, ?, 'session', 'fetched', ?, ?)",
    )
    .bind(&new_id)
    .bind(project_id)
    .bind(&engine)
    .bind(&now)
    .execute(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(new_id)
}
