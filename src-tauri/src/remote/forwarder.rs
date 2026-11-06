//! Stream forwarder (FR-004/FR-006) + history replay (FR-005).
//!
//! When a room is authenticated the agent spawns [`forward_loop`], which
//! subscribes to the in-process [`RemoteBus`], seals each run event with the E2E
//! session key and pushes an envelope onto the room's outbound channel. Only the
//! single BOUND run's events are forwarded (single-session redesign); events for
//! other runs are dropped. Replay reads the persisted `.devdy/runs/<run_id>.log` and
//! sends it in bounded batches before live stream continues (FR-005/AC-07),
//! capped at [`REPLAY_MAX_LINES`] / [`REPLAY_MAX_BYTES`], with inline images and
//! oversized strings stripped so a phone can afford to parse it.

use crate::db::Db;
use crate::remote::bus::{RemoteBus, RemoteRunEvent};
use crate::remote::outbound::OutboundTx;
use crate::remote::protocol::{
    ClaudeAccountChoice, ClaudeAccountUsage, DiscoveredModel, EngineOption, Envelope, FrameType,
    ModelOption,
    OutputLine, ProjectInfo,
    RunInfo,
    SlashCommandInfo,
    StreamPayload,
};
use remote_e2e::Session;
use sqlx::Row;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Cap on how many project files a `list_project_files` push returns.
const PROJECT_FILES_MAX: usize = 500;

/// Cap on how many log lines a single replay returns, to protect the link
/// (BR-006). Newer-than-cap logs are truncated from the FRONT (oldest dropped)
/// so the Controller still lands on the most recent context.
pub const REPLAY_MAX_LINES: usize = 2000;
/// Total bytes (after [`slim_replay_line`]) a single replay may carry. Like the
/// line cap, the newest lines win. The phone decrypts and parses the whole
/// history in one go on its main thread, so this bounds that work too.
pub const REPLAY_MAX_BYTES: usize = 2 * 1024 * 1024;
/// Lines per replay batch envelope.
const REPLAY_BATCH: usize = 200;
/// Approximate bytes per replay batch envelope (raw line bytes; a single line
/// larger than this still ships, alone in its batch).
const REPLAY_BATCH_BYTES: usize = 256 * 1024;
/// Longest string value (or non-JSON line) replay ships verbatim; longer ones
/// are cut with a `…[truncated N bytes]` marker.
const REPLAY_MAX_STRING: usize = 64 * 1024;
/// Stands in for an inline image on the Controller. Swapped in as a `text`
/// block, which the stream parser shows as text in a user bubble or a
/// tool result instead of trying to decode an image.
const REPLAY_IMAGE_PLACEHOLDER: &str = "[image omitted on remote]";

/// Monotonic per-room sequence for `seq` on outbound data frames (SRS §5.1).
#[derive(Default)]
pub struct SeqCounter(AtomicU64);

impl SeqCounter {
    pub fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed)
    }
}

/// Seal a stream payload into an envelope, or `None` if serialization/sealing
/// fails (never panics on the network path).
pub(crate) fn seal_stream(
    session: &Session,
    room_id: &str,
    payload: &StreamPayload,
    seq: u64,
) -> Option<Envelope> {
    let json = serde_json::to_vec(payload).ok()?;
    let cipher = session.seal(&json);
    Some(Envelope::data(FrameType::Stream, room_id, cipher, seq))
}

/// Map a tapped [`RemoteRunEvent`] to the wire [`StreamPayload`].
fn to_payload(ev: RemoteRunEvent) -> StreamPayload {
    match ev {
        RemoteRunEvent::Event { run_id, event } => StreamPayload::Stream { run_id, event },
        RemoteRunEvent::Output {
            run_id,
            line,
            is_stderr,
        } => StreamPayload::Output {
            run_id,
            line,
            is_stderr,
        },
        RemoteRunEvent::PermissionRequest {
            run_id,
            request_id,
            tool,
            input,
            cwd,
            once_only,
        } => StreamPayload::PermissionRequest {
            run_id,
            request_id,
            tool,
            input,
            cwd,
            once_only,
        },
        RemoteRunEvent::Done { run_id, status } => StreamPayload::Done { run_id, status },
        RemoteRunEvent::Meta {
            run_id,
            engine,
            model,
            permission_mode,
        } => StreamPayload::RunMeta {
            run_id,
            engine,
            model,
            permission_mode,
            // The bus carries composer selections only; the account is run state
            // and rides the DB-backed `send_run_meta` push instead.
            claude_account_id: None,
        },
        RemoteRunEvent::PermissionResolved { run_id, request_id } => {
            StreamPayload::PermissionResolved {
                run_id,
                request_id,
                accepted: true,
                reason: Some("resolved_on_host".to_string()),
                remembered: None,
            }
        }
    }
}

/// How long output lines may sit in the coalescing buffer before being flushed.
/// Short enough to stay imperceptible in a live transcript, long enough that a
/// burst of log lines collapses into one frame instead of dozens.
const OUTPUT_COALESCE_WINDOW: Duration = Duration::from_millis(50);
/// Flush early once this many lines are buffered, so a firehose does not build a
/// single oversized frame.
const OUTPUT_COALESCE_MAX_LINES: usize = 256;

/// How long a permission prompt waits before it goes to the phone. The
/// desktop answers prompts covered by the project's standing allow / deny
/// choices itself, but only after the Host has already put them on the bus:
/// forwarded at once, they flashed on the phone and vanished before anyone
/// could tap. A prompt still open after this is a real question.
const PROMPT_SETTLE: Duration = Duration::from_millis(800);

/// Whether a prompt for `tool` waits [`PROMPT_SETTLE`] first. AskUserQuestion
/// is never auto-answered on the desktop, so it goes out at once.
fn settles_first(tool: &str) -> bool {
    tool != "AskUserQuestion"
}

/// What a running forward loop is allowed to forward.
pub struct ForwardScope {
    /// Runs whose live transcript is streamed; mutated by `open_run`/`close_run`.
    pub subscriptions: crate::remote::RunSubscriptions,
    /// The run the session link was minted for. Always present in
    /// `subscriptions`, and — for a legacy Controller — the only run at all.
    pub initial_run_id: String,
    /// Whether the Controller can see runs beyond `initial_run_id`
    /// ([`FEATURE_MULTI_RUN`]).
    pub multi_run: bool,
    /// Whether the Controller understands [`StreamPayload::OutputBatch`].
    pub batch_output: bool,
}

/// Forward live run events to the Controller until the room closes.
///
/// Runs until `out` is closed (room gone) or the bus closes.
///
/// **Two tiers of forwarding.** Permission requests, run completion and meta
/// changes are low-volume and worth seeing wherever the user is, so with
/// `multi_run` they are forwarded for EVERY run — that is what lets the phone
/// approve a tool in a run it is not currently watching. The high-volume live
/// transcript (stream events, console output) is forwarded only for runs the
/// Controller has actually opened. A legacy Controller gets neither: both tiers
/// collapse to `initial_run_id`, exactly as before multi-run.
///
/// A `broadcast` lag is tolerated: we skip the missed window, tell the
/// Controller so it can backfill with `request_history` (FR-005/FR-012), and
/// keep going.
///
/// Output lines are coalesced into [`StreamPayload::OutputBatch`] frames when the
/// Controller advertised [`FEATURE_OUTPUT_BATCH`]. Ordering is preserved
/// per run: a run's buffer is flushed before any other event of THAT run is
/// sealed, so a batch is exactly the `Output` frames it stands in for.
///
/// Live stream/output goes out via [`OutboundTx::send_lossy`] — under congestion
/// it is dropped rather than queued forever, because the Controller can backfill
/// with `request_history`. Everything else (permission requests, run status,
/// meta, usage) uses the non-droppable path.
pub async fn forward_loop(
    bus: RemoteBus,
    session: Arc<Session>,
    room_id: String,
    out: OutboundTx,
    seq: Arc<SeqCounter>,
    db: Db,
    scope: ForwardScope,
) {
    let mut rx = bus.subscribe();
    // Buffered output lines per run, each in emission order.
    let mut buffered: HashMap<String, Vec<OutputLine>> = HashMap::new();
    let mut flush_at: Option<tokio::time::Instant> = None;

    loop {
        let event = tokio::select! {
            r = rx.recv() => Some(r),
            // Only armed while lines are buffered; otherwise this branch parks
            // forever and the loop is a plain `rx.recv()`.
            _ = async {
                match flush_at {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending::<()>().await,
                }
            } => None,
        };

        let Some(received) = event else {
            // Coalescing window elapsed — drain every run's buffer.
            if flush_all(&mut buffered, &session, &room_id, &out, &seq).is_err() {
                break;
            }
            flush_at = None;
            continue;
        };

        match received {
            Ok(ev) => {
                let run_id = ev.run_id().to_string();
                let streaming = scope.streams(&run_id).await;
                let is_transcript = matches!(
                    ev,
                    RemoteRunEvent::Event { .. } | RemoteRunEvent::Output { .. }
                );

                // Tier 1: the live transcript, only for runs the user opened.
                if is_transcript && !streaming {
                    continue;
                }
                // Tier 2: signals. Always for a multi-run Controller, otherwise
                // only for the one run it can render.
                if !is_transcript && !scope.signals(&run_id) {
                    continue;
                }

                // Accumulate output lines instead of sealing one frame per line.
                if scope.batch_output {
                    if let RemoteRunEvent::Output {
                        line, is_stderr, ..
                    } = &ev
                    {
                        let lines = buffered.entry(run_id.clone()).or_default();
                        lines.push(OutputLine {
                            line: line.clone(),
                            is_stderr: *is_stderr,
                        });
                        if lines.len() >= OUTPUT_COALESCE_MAX_LINES {
                            if flush_run(&mut buffered, &run_id, &session, &room_id, &out, &seq)
                                .is_err()
                            {
                                break;
                            }
                        } else if flush_at.is_none() {
                            flush_at = Some(tokio::time::Instant::now() + OUTPUT_COALESCE_WINDOW);
                        }
                        continue;
                    }
                    // Another event of THIS run must not overtake its buffered
                    // output — drain that run first so its transcript stays
                    // ordered. Other runs are independent timelines.
                    if flush_run(&mut buffered, &run_id, &session, &room_id, &out, &seq).is_err() {
                        break;
                    }
                }

                // A prompt the desktop may still auto-answer: send it only if it
                // is still open once that had its chance.
                if let RemoteRunEvent::PermissionRequest { request_id, tool, .. } = &ev {
                    if settles_first(tool) {
                        let request_id = request_id.clone();
                        let payload = to_payload(ev);
                        let (session, room_id, out, seq) =
                            (session.clone(), room_id.clone(), out.clone(), seq.clone());
                        tokio::spawn(async move {
                            tokio::time::sleep(PROMPT_SETTLE).await;
                            if crate::runs::permission::peek_pending(&run_id, &request_id).is_none() {
                                return;
                            }
                            if let Some(env) = seal_stream(&session, &room_id, &payload, seq.next()) {
                                let _ = out.send(env);
                            }
                        });
                        continue;
                    }
                }

                let is_done = matches!(ev, RemoteRunEvent::Done { .. });
                // Live transcript is backfillable; status/permission/meta is not.
                let payload = to_payload(ev);
                if let Some(env) = seal_stream(&session, &room_id, &payload, seq.next()) {
                    let sent = if is_transcript {
                        out.send_lossy(env)
                    } else {
                        out.send(env)
                    };
                    if sent.is_err() {
                        break; // room/writer gone
                    }
                }
                if is_done {
                    // Refresh usage badges after a turn (the sidecar has captured
                    // the turn's /usage by now) — only for a run being watched,
                    // since the badges are rendered per open run.
                    if streaming {
                        send_plan_usage(&db, &session, &room_id, &out, &seq, &run_id).await;
                    }
                    // A finished run changes the browse list; refresh it so the
                    // Controller's run list does not show a stale status.
                    send_run_list(&db, &session, &room_id, &out, &seq, scope.list_filter()).await;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(event = "remote_forward_lagged", skipped = n);
                // Events were dropped from the bus ring before we read them, so
                // the Controller's transcript now has a silent hole. Tell it, so
                // it can backfill with request_history (FR-005/FR-012) instead of
                // showing a plausible-looking but incomplete turn.
                let payload = StreamPayload::Notice {
                    run_id: None,
                    text: format!("stream_gap:{n}"),
                };
                if let Some(env) = seal_stream(&session, &room_id, &payload, seq.next()) {
                    if out.send(env).is_err() {
                        break;
                    }
                }
                continue;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }

    // Best-effort final drain so a clean shutdown does not swallow the tail.
    let _ = flush_all(&mut buffered, &session, &room_id, &out, &seq);
}

impl ForwardScope {
    /// Whether the Controller is watching `run_id`'s live transcript.
    async fn streams(&self, run_id: &str) -> bool {
        if !self.multi_run {
            return run_id == self.initial_run_id;
        }
        self.subscriptions.lock().await.contains(run_id)
    }

    /// Whether permission requests / status / meta for `run_id` should be sent.
    fn signals(&self, run_id: &str) -> bool {
        self.multi_run || run_id == self.initial_run_id
    }

    /// `None` for a multi-run Controller (list every run), otherwise the single
    /// run it is allowed to see.
    fn list_filter(&self) -> Option<&str> {
        if self.multi_run {
            None
        } else {
            Some(&self.initial_run_id)
        }
    }
}

/// Seal and send one run's buffered output lines, leaving its buffer empty.
fn flush_run(
    buffered: &mut HashMap<String, Vec<OutputLine>>,
    run_id: &str,
    session: &Session,
    room_id: &str,
    out: &OutboundTx,
    seq: &SeqCounter,
) -> Result<(), ()> {
    let Some(lines) = buffered.remove(run_id).filter(|l| !l.is_empty()) else {
        return Ok(());
    };
    let payload = StreamPayload::OutputBatch {
        run_id: run_id.to_string(),
        lines,
    };
    match seal_stream(session, room_id, &payload, seq.next()) {
        Some(env) => out.send_lossy(env),
        // A payload that cannot be sealed is dropped, not fatal.
        None => Ok(()),
    }
}

/// Drain every run's buffer. `Err(())` means the room/writer is gone.
fn flush_all(
    buffered: &mut HashMap<String, Vec<OutputLine>>,
    session: &Session,
    room_id: &str,
    out: &OutboundTx,
    seq: &SeqCounter,
) -> Result<(), ()> {
    let run_ids: Vec<String> = buffered.keys().cloned().collect();
    for run_id in run_ids {
        flush_run(buffered, &run_id, session, room_id, out, seq)?;
    }
    Ok(())
}

/// Cap on how many runs a browse list returns PER PROJECT. Capping per project
/// (not overall) keeps a quiet project from vanishing behind a busy one; beyond
/// this the Controller is scrolling history, which belongs to the desktop.
pub const RUN_LIST_PER_PROJECT_MAX: i64 = 50;

/// Build a run-list snapshot in the desktop History order: pinned first, then
/// last activity (`commands::github::list_runs`). Non-secret metadata only.
///
/// Conductor workers are left out, as on the desktop: they live in their
/// conductor's worker panel, not in History.
///
/// `only_run_id` scopes the list to a single run — used for a legacy Controller
/// that can only render the run its link was minted for. `None` lists every
/// project's runs (capped at [`RUN_LIST_PER_PROJECT_MAX`] each), which is what
/// the run browser needs; the trust boundary is device approval, not per-run
/// share (SRS BR-005 v1.1).
pub async fn build_run_list(db: &Db, only_run_id: Option<&str>) -> Vec<RunInfo> {
    const ACTIVITY: &str = "COALESCE(r.last_activity_at, r.finished_at, r.started_at, r.created_at)";
    let columns = format!(
        "SELECT r.id AS id, r.project_id AS project_id, p.name AS project_name,
                r.title AS title, r.type AS run_type, r.ref_number AS ref_number,
                r.status AS status, r.engine AS engine, r.created_at AS created_at,
                {ACTIVITY} AS updated_at, r.finished_at AS finished_at, r.pinned AS pinned
         FROM runs r JOIN projects p ON p.id = r.project_id"
    );
    let rows = match only_run_id {
        Some(run_id) => sqlx::query(&format!("{columns} WHERE r.id = ?"))
            .bind(run_id)
            .fetch_all(db)
            .await,
        None => sqlx::query(&format!(
            "SELECT * FROM (
                 SELECT *, ROW_NUMBER() OVER (
                     PARTITION BY project_id ORDER BY pinned DESC, updated_at DESC
                 ) AS rank_in_project
                 FROM ({columns} WHERE r.conductor_run_id IS NULL)
             )
             WHERE rank_in_project <= ?
             ORDER BY pinned DESC, updated_at DESC"
        ))
        .bind(RUN_LIST_PER_PROJECT_MAX)
        .fetch_all(db)
        .await,
    }
    .unwrap_or_default();
    rows.into_iter()
        .map(|r| RunInfo {
            id: r.get("id"),
            project_id: r.get("project_id"),
            project_name: r.get("project_name"),
            title: r.get("title"),
            run_type: r.get("run_type"),
            ref_number: r.get("ref_number"),
            status: r.get("status"),
            engine: r.get("engine"),
            created_at: r.get("created_at"),
            updated_at: r.get("updated_at"),
            finished_at: r.get("finished_at"),
            pinned: r.get::<i64, _>("pinned") != 0,
        })
        .collect()
}

/// Seal + send a fresh (scoped) run-list snapshot to the Controller.
pub async fn send_run_list(
    db: &Db,
    session: &Session,
    room_id: &str,
    out: &OutboundTx,
    seq: &SeqCounter,
    only_run_id: Option<&str>,
) {
    let runs = build_run_list(db, only_run_id).await;
    let payload = StreamPayload::RunList { runs };
    if let Some(env) = seal_stream(session, room_id, &payload, seq.next()) {
        let _ = out.send(env);
    }
}

/// Seal + send the bound run's engine/model/permission-mode snapshot. Prefers the
/// live in-memory selection (what the desktop composer last pushed); falls back to
/// the run's persisted engine so a controller that pairs before the composer has
/// pushed anything still lands on the correct engine (not a blank/default).
pub async fn send_run_meta(
    db: &Db,
    store: &crate::remote::RunMetaStore,
    session: &Session,
    room_id: &str,
    out: &OutboundTx,
    seq: &SeqCounter,
    bound_run_id: &str,
) {
    let mut meta = store.get(bound_run_id).unwrap_or_default();
    if meta.engine.as_deref().unwrap_or("").trim().is_empty() {
        meta.engine = sqlx::query_scalar::<_, Option<String>>("SELECT engine FROM runs WHERE id = ?")
            .bind(bound_run_id)
            .fetch_optional(db)
            .await
            .ok()
            .flatten()
            .flatten();
    }
    // Which managed account the run executes with. Read from the run row rather
    // than the meta store: the account is run state the desktop writes directly,
    // not a composer selection the store caches. NULL becomes "" — the global
    // ~/.claude — which the Controller must be able to tell from "not reported".
    let claude_account_id = sqlx::query_scalar::<_, Option<String>>(
        "SELECT claude_account_id FROM runs WHERE id = ?",
    )
    .bind(bound_run_id)
    .fetch_optional(db)
    .await
    .ok()
    .map(|row| row.flatten().unwrap_or_default());

    let payload = StreamPayload::RunMeta {
        run_id: bound_run_id.to_string(),
        engine: meta.engine,
        model: meta.model,
        permission_mode: meta.permission_mode,
        claude_account_id,
    };
    if let Some(env) = seal_stream(session, room_id, &payload, seq.next()) {
        let _ = out.send(env);
    }
}

/// Static slash-command list surfaced to the Controller composer. Kept minimal
/// in v1 (the desktop discovers live commands per engine; the controller uses
/// this baseline until parity is deepened).
pub fn slash_command_list() -> Vec<SlashCommandInfo> {
    ["compact", "clear", "review"]
        .into_iter()
        .map(|name| SlashCommandInfo {
            name: name.to_string(),
            description: None,
        })
        .collect()
}

/// Seal + send the slash-command list to the Controller.
pub async fn send_slash_command_list(
    session: &Session,
    room_id: &str,
    out: &OutboundTx,
    seq: &SeqCounter,
) {
    let payload = StreamPayload::SlashCommandList {
        commands: slash_command_list(),
    };
    if let Some(env) = seal_stream(session, room_id, &payload, seq.next()) {
        let _ = out.send(env);
    }
}

/// Static engine + model options mirroring the desktop's `MODEL_OPTIONS`.
pub fn engine_model_options() -> Vec<EngineOption> {
    let claude = EngineOption {
        id: "claude".to_string(),
        label: "Claude".to_string(),
        default_model: None,
        models: [
            ("fable", "Fable 5 (1M)"),
            ("opus", "Opus (200K)"),
            ("opus[1m]", "Opus (1M)"),
            ("sonnet", "Sonnet (200K)"),
            ("sonnet[1m]", "Sonnet (1M)"),
            ("haiku", "Haiku"),
        ]
        .into_iter()
        .map(|(id, label)| ModelOption {
            id: id.to_string(),
            label: label.to_string(),
        })
        .collect(),
    };
    let codex = EngineOption {
        id: "codex".to_string(),
        label: "Codex".to_string(),
        default_model: None,
        models: [
            ("gpt-5.5", "gpt-5.5"),
            ("gpt-5.4", "gpt-5.4"),
            ("gpt-5.3-codex", "gpt-5.3-codex"),
            ("gpt-5.2-codex", "gpt-5.2-codex"),
            ("gpt-5.1-codex-mini", "gpt-5.1-codex-mini"),
        ]
        .into_iter()
        .map(|(id, label)| ModelOption {
            id: id.to_string(),
            label: label.to_string(),
        })
        .collect(),
    };
    vec![claude, codex]
}

/// Seal + send the engine/model options to the Controller.
///
/// Ships the models DISCOVERED from the account alongside the legacy curated
/// table. The controller merges them with its own curated table using the same
/// rules the desktop composer uses, so a model refreshed on the desktop shows up
/// remotely without a controller redeploy — the curated table alone would leave
/// the two selectors offering different lists.
pub async fn send_engine_model_options(
    db: &Db,
    session: &Session,
    room_id: &str,
    out: &OutboundTx,
    seq: &SeqCounter,
) {
    let codex = crate::commands::models::active_discovered_models(db).await;
    let to_wire = |m: crate::commands::models::ModelOption| DiscoveredModel {
        value: m.value,
        label: m.label,
        description: m.description,
    };
    // The accounts a run may be switched to, mirroring the desktop run-settings
    // menu. Same ordering as the desktop list (default first, then by label).
    let rows = sqlx::query(
        "SELECT id, label, is_default, email FROM claude_accounts
         ORDER BY is_default DESC, label COLLATE NOCASE ASC",
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();
    let claude_accounts = rows
        .iter()
        .map(|r| ClaudeAccountChoice {
            id: r.get("id"),
            label: r.get("label"),
            is_default: r.get::<i64, _>("is_default") != 0,
            email: r.get("email"),
        })
        .collect();

    let payload = StreamPayload::EngineModelOptions {
        engines: engine_model_options(),
        // Claude's list is curated in the shared frontend bundle, so the
        // controller already has it — nothing to discover or ship. The field
        // stays on the wire (empty, hence skipped) for older controllers.
        claude_models: Vec::new(),
        codex_models: codex.into_iter().map(to_wire).collect(),
        claude_accounts,
    };
    if let Some(env) = seal_stream(session, room_id, &payload, seq.next()) {
        let _ = out.send(env);
    }
}

/// Seal + send the subscription plan-usage badges (Claude + Codex) to the
/// Controller, mirroring the desktop `BudgetBadge`. Best-effort: a provider with
/// no captured snapshot serializes to its `disabled` shape (a quiet, zero row).
/// Reuses `stats::budget_status`, the off-Tauri-path badge builder.
pub async fn send_plan_usage(
    db: &Db,
    session: &Session,
    room_id: &str,
    out: &OutboundTx,
    seq: &SeqCounter,
    bound_run_id: &str,
) {
    // Which account this run actually executes with (its snapshotted account,
    // else the current default). Used to mark the row that matters right now.
    let account_id = match crate::commands::claude_accounts::run_account_id(db, bound_run_id).await {
        Some(id) => Some(id),
        None => crate::commands::claude_accounts::default_account_id(db).await,
    };
    let claude_key = crate::commands::stats::claude_plan_usage_key(account_id.as_deref());
    let claude = crate::commands::stats::budget_status(db, &claude_key).await;
    let codex = crate::commands::stats::budget_status(db, "plan_usage_codex").await;

    // Desktop BudgetBadge parity: one row per managed account, not just the
    // run's own. With several accounts in play the interesting question is
    // usually "which account still has headroom", which a single row cannot
    // answer. Ordered like the desktop list (default first, then by label).
    let rows = sqlx::query(
        "SELECT id, label FROM claude_accounts
         ORDER BY is_default DESC, label COLLATE NOCASE ASC",
    )
    .fetch_all(db)
    .await
    .unwrap_or_default();
    let mut claude_accounts = Vec::with_capacity(rows.len());
    for r in &rows {
        let id: String = r.get("id");
        let key = crate::commands::stats::claude_plan_usage_key(Some(&id));
        let budget = crate::commands::stats::budget_status(db, &key).await;
        claude_accounts.push(ClaudeAccountUsage {
            active: account_id.as_deref() == Some(id.as_str()),
            label: r.get("label"),
            id,
            budget: serde_json::to_value(&budget).ok(),
        });
    }

    let claude_account = claude_accounts
        .iter()
        .find(|a| a.active)
        .map(|a| a.label.clone());

    let payload = StreamPayload::PlanUsage {
        claude: serde_json::to_value(&claude).ok(),
        codex: serde_json::to_value(&codex).ok(),
        claude_account,
        claude_accounts,
    };
    if let Some(env) = seal_stream(session, room_id, &payload, seq.next()) {
        let _ = out.send(env);
    }
}

/// Seal + send the desktop's theme + color palette so the Controller renders
/// with the same look. Unset keys fall back to the desktop defaults.
pub async fn send_appearance(
    db: &Db,
    session: &Session,
    room_id: &str,
    out: &OutboundTx,
    seq: &SeqCounter,
) {
    let read = |key: &'static str, fallback: &'static str| async move {
        sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(db)
            .await
            .ok()
            .flatten()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| fallback.to_string())
    };
    let payload = StreamPayload::Appearance {
        theme: read("theme", "system").await,
        color_theme: read("color_theme", "default").await,
    };
    if let Some(env) = seal_stream(session, room_id, &payload, seq.next()) {
        let _ = out.send(env);
    }
}

/// List files under the bound run's project path (bounded, relative paths). Skips
/// dot-directories (e.g. `.git`, `.devdy`) and caps the result to keep the link
/// light. Used for @mention completion in the controller composer.
pub async fn send_project_file_list(
    db: &Db,
    session: &Session,
    room_id: &str,
    out: &OutboundTx,
    seq: &SeqCounter,
    bound_run_id: &str,
) {
    let project_path: Option<String> = sqlx::query_scalar(
        "SELECT p.path FROM runs r JOIN projects p ON p.id = r.project_id WHERE r.id = ?",
    )
    .bind(bound_run_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    let files = match project_path {
        Some(root) => list_project_files(&root),
        None => Vec::new(),
    };
    let payload = StreamPayload::ProjectFileList {
        run_id: bound_run_id.to_string(),
        files,
    };
    if let Some(env) = seal_stream(session, room_id, &payload, seq.next()) {
        let _ = out.send(env);
    }
}

/// Walk `root` breadth-first, returning up to [`PROJECT_FILES_MAX`] relative file
/// paths. Dot-directories are skipped. Best-effort — I/O errors yield fewer/no
/// entries rather than failing.
fn list_project_files(root: &str) -> Vec<String> {
    let root_path = PathBuf::from(root);
    let mut out: Vec<String> = Vec::new();
    let mut queue: std::collections::VecDeque<PathBuf> =
        std::collections::VecDeque::from([root_path.clone()]);
    while let Some(dir) = queue.pop_front() {
        if out.len() >= PROJECT_FILES_MAX {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue; // skip .git / .devdy / dotfiles
            }
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                queue.push_back(path);
            } else if ft.is_file() {
                if let Ok(rel) = path.strip_prefix(&root_path) {
                    out.push(rel.to_string_lossy().to_string());
                    if out.len() >= PROJECT_FILES_MAX {
                        break;
                    }
                }
            }
        }
    }
    out
}

/// Build the project list the Controller can create a new run in (Phase 2).
pub async fn build_project_list(db: &Db) -> Vec<ProjectInfo> {
    // The desktop sidebar's order, so the phone lists projects the same way.
    let rows = sqlx::query(&format!(
        "SELECT p.id AS id, p.name AS name, p.path AS path
         FROM projects p LEFT JOIN runs r ON r.project_id = p.id
         GROUP BY p.id {}",
        crate::commands::projects::PROJECT_ORDER_BY
    ))
    .fetch_all(db)
    .await
    .unwrap_or_default();
    rows.into_iter()
        .map(|r| ProjectInfo {
            id: r.get("id"),
            name: r.get("name"),
            path: r.get("path"),
        })
        .collect()
}

/// Seal + send the project list to the Controller.
pub async fn send_project_list(
    db: &Db,
    session: &Session,
    room_id: &str,
    out: &OutboundTx,
    seq: &SeqCounter,
) {
    let projects = build_project_list(db).await;
    let payload = StreamPayload::ProjectList { projects };
    if let Some(env) = seal_stream(session, room_id, &payload, seq.next()) {
        let _ = out.send(env);
    }
}

/// The conventional log path for a run inside a project (INT-004).
pub fn run_log_path(project_path: &str, run_id: &str) -> PathBuf {
    PathBuf::from(project_path)
        .join(".devdy")
        .join("runs")
        .join(format!("{run_id}.log"))
}

/// Cut `s` to at most [`REPLAY_MAX_STRING`] bytes (on a char boundary) plus a
/// marker saying how much was dropped. `None` when it already fits.
fn truncate_replay_str(s: &str) -> Option<String> {
    if s.len() <= REPLAY_MAX_STRING {
        return None;
    }
    let mut cut = REPLAY_MAX_STRING;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    Some(format!("{}…[truncated {} bytes]", &s[..cut], s.len() - cut))
}

/// An inline (base64) image block: Claude's `{"type":"image","source":{"data":..}}`
/// or a `data:` URL under `url` / `image_url`.
fn is_inline_image(map: &serde_json::Map<String, serde_json::Value>) -> bool {
    if map.get("type").and_then(|t| t.as_str()) != Some("image") {
        return false;
    }
    let has_data = map
        .get("source")
        .and_then(|s| s.get("data"))
        .is_some_and(|d| d.is_string());
    let data_url = ["url", "image_url"].iter().any(|k| {
        map.get(*k)
            .and_then(|u| u.as_str())
            .is_some_and(|u| u.starts_with("data:"))
    });
    has_data || data_url
}

/// Strip inline images and truncate oversized strings in place. Returns whether
/// anything changed, so untouched lines ship byte-for-byte.
fn slim_value(v: &mut serde_json::Value) -> bool {
    use serde_json::Value;
    match v {
        Value::Object(map) if is_inline_image(map) => {
            *v = serde_json::json!({ "type": "text", "text": REPLAY_IMAGE_PLACEHOLDER });
            true
        }
        Value::Object(map) => map.values_mut().fold(false, |acc, x| slim_value(x) | acc),
        Value::Array(items) => items.iter_mut().fold(false, |acc, x| slim_value(x) | acc),
        Value::String(s) => match truncate_replay_str(s) {
            Some(t) => {
                *s = t;
                true
            }
            None => false,
        },
        _ => false,
    }
}

/// Shrink one log line for the Controller: images (base64 screenshots run to
/// hundreds of KB per line) become a text placeholder and huge strings are cut.
/// Non-JSON lines are only truncated. Desktop views read the log file directly
/// and keep full fidelity; this only shapes what goes over the relay.
fn slim_replay_line(line: &str) -> String {
    let Ok(mut v) = serde_json::from_str::<serde_json::Value>(line) else {
        return truncate_replay_str(line).unwrap_or_else(|| line.to_string());
    };
    if !slim_value(&mut v) {
        return line.to_string();
    }
    serde_json::to_string(&v).unwrap_or_else(|_| line.to_string())
}

/// Slim the log's lines and keep the newest that fit both [`REPLAY_MAX_LINES`]
/// and [`REPLAY_MAX_BYTES`] (the newest line is kept even if it alone is over).
/// Works from the tail so dropped lines are never parsed.
fn select_replay_lines(content: &str) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    let mut bytes = 0usize;
    for raw in content.lines().rev().take(REPLAY_MAX_LINES) {
        let line = slim_replay_line(raw);
        if !kept.is_empty() && bytes + line.len() > REPLAY_MAX_BYTES {
            break;
        }
        bytes += line.len();
        kept.push(line);
    }
    kept.reverse();
    kept
}

/// Split lines into batches of at most [`REPLAY_BATCH`] lines and roughly
/// [`REPLAY_BATCH_BYTES`]; every batch holds at least one line.
fn batch_replay_lines(lines: Vec<String>) -> Vec<Vec<String>> {
    let mut batches: Vec<Vec<String>> = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    let mut cur_bytes = 0usize;
    for line in lines {
        if !cur.is_empty()
            && (cur.len() >= REPLAY_BATCH || cur_bytes + line.len() > REPLAY_BATCH_BYTES)
        {
            batches.push(std::mem::take(&mut cur));
            cur_bytes = 0;
        }
        cur_bytes += line.len();
        cur.push(line);
    }
    if !cur.is_empty() {
        batches.push(cur);
    }
    batches
}

/// Send the persisted log for `run_id` back to the Controller in batches
/// (FR-005/AC-07). Returns the number of lines sent. Lines are slimmed for the
/// phone (no inline images, long strings cut) and capped by count and bytes,
/// newest kept. The final batch carries `done = true`; live stream then
/// continues without overlap because the Controller only started receiving
/// live events after it joined.
pub async fn replay_history(
    session: &Session,
    room_id: &str,
    run_id: &str,
    log_path: &std::path::Path,
    out: &OutboundTx,
    seq: &SeqCounter,
) -> usize {
    let content = std::fs::read_to_string(log_path).unwrap_or_default();
    // BR-006: cap the replay; keep the most recent lines.
    let lines = select_replay_lines(&content);
    let total = lines.len();

    let mut chunks = batch_replay_lines(lines).into_iter().peekable();
    if chunks.peek().is_none() {
        // No history — still signal completion so the Controller stops waiting.
        let payload = StreamPayload::History {
            run_id: run_id.to_string(),
            lines: Vec::new(),
            done: true,
        };
        if let Some(env) = seal_stream(session, room_id, &payload, seq.next()) {
            let _ = out.send(env);
        }
        return 0;
    }

    while let Some(chunk) = chunks.next() {
        let done = chunks.peek().is_none();
        let payload = StreamPayload::History {
            run_id: run_id.to_string(),
            lines: chunk,
            done,
        };
        if let Some(env) = seal_stream(session, room_id, &payload, seq.next()) {
            if out.send(env).is_err() {
                break;
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_questions_skip_the_settle_wait() {
        assert!(!super::settles_first("AskUserQuestion"));
        assert!(super::settles_first("Bash"));
        assert!(super::settles_first("mcp__devdy__notes_create"));
    }

    use super::*;
    use remote_e2e::Keypair;
    use std::io::Write;

    fn test_session() -> Session {
        // Two ephemeral keypairs + a shared psk → a real session key. Both ends
        // derive the same key; here we just need any valid Session to seal with.
        let a = Keypair::generate();
        let b = Keypair::generate();
        // psk must be >= 16 bytes (BLAKE2b keyed key minimum).
        Session::new(b"unit-test-psk-0123456789", &a, &b.public).unwrap()
    }

    fn scope(multi_run: bool, subscribed: &[&str]) -> ForwardScope {
        ForwardScope {
            subscriptions: Arc::new(tokio::sync::Mutex::new(
                subscribed.iter().map(|s| s.to_string()).collect(),
            )),
            initial_run_id: "r_linked".to_string(),
            multi_run,
            batch_output: false,
        }
    }

    async fn list_db() -> Db {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&db).await.unwrap();
        db
    }

    async fn add_project(db: &Db, id: &str, name: &str, position: i64) {
        sqlx::query("INSERT INTO projects (id, name, path, created_at, position) VALUES (?, ?, ?, '2026-01-01', ?)")
            .bind(id)
            .bind(name)
            .bind(format!("/tmp/{id}"))
            .bind(position)
            .execute(db)
            .await
            .unwrap();
    }

    async fn add_run(db: &Db, id: &str, project: &str, activity: &str, pinned: bool, conductor: Option<&str>) {
        sqlx::query(
            "INSERT INTO runs (id, project_id, type, created_at, last_activity_at, pinned, conductor_run_id)
             VALUES (?, ?, 'session', '2026-01-01', ?, ?, ?)",
        )
        .bind(id)
        .bind(project)
        .bind(activity)
        .bind(pinned as i64)
        .bind(conductor)
        .execute(db)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn run_list_follows_the_desktop_history_order() {
        let db = list_db().await;
        add_project(&db, "p", "proj", 0).await;
        add_run(&db, "old_pinned", "p", "2026-01-02", true, None).await;
        add_run(&db, "newest", "p", "2026-03-01", false, None).await;
        add_run(&db, "older", "p", "2026-02-01", false, None).await;
        // A conductor worker lives in its conductor's panel, not in History.
        add_run(&db, "worker", "p", "2026-04-01", false, Some("newest")).await;

        let runs = build_run_list(&db, None).await;
        let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["old_pinned", "newest", "older"]);
        assert!(runs[0].pinned);
        assert!(!runs[1].pinned);
    }

    #[tokio::test]
    async fn run_list_caps_each_project_without_starving_a_quiet_one() {
        let db = list_db().await;
        add_project(&db, "busy", "busy", 0).await;
        add_project(&db, "quiet", "quiet", 1).await;
        add_run(&db, "quiet_run", "quiet", "2025-01-01", false, None).await;
        for i in 0..RUN_LIST_PER_PROJECT_MAX + 5 {
            add_run(&db, &format!("b{i:03}"), "busy", &format!("2026-01-01T00:{:02}:{:02}", i / 60, i % 60), false, None)
                .await;
        }
        // A pinned run is never the one cut, however old.
        add_run(&db, "busy_pinned", "busy", "2020-01-01", true, None).await;

        let runs = build_run_list(&db, None).await;
        let busy = runs.iter().filter(|r| r.project_id == "busy").count() as i64;
        assert_eq!(busy, RUN_LIST_PER_PROJECT_MAX);
        assert!(runs.iter().any(|r| r.id == "quiet_run"));
        assert!(runs.iter().any(|r| r.id == "busy_pinned"));
    }

    #[tokio::test]
    async fn project_list_follows_the_desktop_sidebar_order() {
        let db = list_db().await;
        // Manual (drag-and-drop) order wins over the name.
        add_project(&db, "z", "Zeta", 0).await;
        add_project(&db, "a", "alpha", 2).await;
        add_project(&db, "m", "Mu", 1).await;
        // Same position: more runs first, then the name.
        add_project(&db, "y", "Yota", 3).await;
        add_project(&db, "b", "beta", 3).await;
        add_project(&db, "c", "Gamma", 3).await;
        add_run(&db, "r1", "y", "2026-01-01", false, None).await;

        let projects = build_project_list(&db).await;
        let ids: Vec<&str> = projects.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["z", "m", "a", "y", "b", "c"]);
    }

    #[test]
    fn host_side_resolution_maps_to_an_accepted_permission_resolved() {
        let payload = to_payload(RemoteRunEvent::PermissionResolved {
            run_id: "r_1".to_string(),
            request_id: "p_1".to_string(),
        });
        let StreamPayload::PermissionResolved { run_id, request_id, accepted, reason, .. } = payload
        else {
            panic!("expected permission_resolved");
        };
        assert_eq!((run_id.as_str(), request_id.as_str()), ("r_1", "p_1"));
        assert!(accepted, "the Controller only drops accepted resolutions");
        assert_eq!(reason.as_deref(), Some("resolved_on_host"));
    }

    #[tokio::test]
    async fn legacy_scope_sees_only_the_linked_run() {
        let s = scope(false, &["r_linked", "r_other"]);
        assert!(s.streams("r_linked").await);
        // Even a stale subscription cannot widen a legacy controller's view: it
        // renders one run, so anything else would be invisible noise.
        assert!(!s.streams("r_other").await);
        assert!(s.signals("r_linked"));
        assert!(!s.signals("r_other"));
        assert_eq!(s.list_filter(), Some("r_linked"));
    }

    #[tokio::test]
    async fn multi_run_scope_streams_only_open_runs_but_signals_all() {
        let s = scope(true, &["r_linked"]);
        assert!(s.streams("r_linked").await);
        assert!(
            !s.streams("r_other").await,
            "an unopened run must not stream its transcript"
        );
        // ...yet its permission prompts and status changes still arrive — that is
        // what lets the phone approve a tool in a run it is not watching.
        assert!(s.signals("r_other"));
        assert_eq!(s.list_filter(), None, "the run browser lists every run");
    }

    #[tokio::test]
    async fn opening_a_run_takes_effect_without_restarting_the_loop() {
        let s = scope(true, &["r_linked"]);
        assert!(!s.streams("r_new").await);
        // The handler mutates the same shared set the forwarder reads.
        s.subscriptions.lock().await.insert("r_new".to_string());
        assert!(s.streams("r_new").await);
        s.subscriptions.lock().await.remove("r_new");
        assert!(!s.streams("r_new").await);
    }

    /// Unique temp path in the OS temp dir (no external tempfile dep).
    fn temp_log(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "devdy-remote-test-{}-{}.log",
            tag,
            uuid::Uuid::new_v4().simple()
        ));
        p
    }

    #[test]
    fn replay_caps_to_max_lines_keeping_newest() {
        // Build a session for opening the sealed frames to verify content.
        let a = Keypair::generate();
        let b = Keypair::generate();
        let psk = b"replay-test-psk-0123456789";
        let host = Session::new(psk, &a, &b.public).unwrap();
        let ctrl = Session::new(psk, &b, &a.public).unwrap();

        // Log with more than REPLAY_MAX_LINES lines.
        let path = temp_log("replay");
        let mut f = std::fs::File::create(&path).unwrap();
        let n = REPLAY_MAX_LINES + 500;
        for i in 0..n {
            writeln!(f, "line-{i}").unwrap();
        }
        drop(f);

        let (tx, mut rx) = crate::remote::outbound::channel();
        let seq = SeqCounter::default();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let sent = rt.block_on(replay_history(
            &host, "rm_1", "r_1", &path, &tx, &seq,
        ));
        drop(tx);
        let _ = std::fs::remove_file(&path);

        assert_eq!(sent, REPLAY_MAX_LINES, "capped to REPLAY_MAX_LINES");

        // Reassemble the replayed lines by opening every History frame.
        let mut got: Vec<String> = Vec::new();
        let mut saw_done = false;
        while let Ok(env) = rx.try_recv() {
            let pt = ctrl.open(env.cipher.as_deref().unwrap()).unwrap();
            let payload: StreamPayload = serde_json::from_slice(&pt).unwrap();
            if let StreamPayload::History { lines, done, .. } = payload {
                got.extend(lines);
                saw_done = done || saw_done;
            }
        }
        assert!(saw_done, "final batch flagged done");
        assert_eq!(got.len(), REPLAY_MAX_LINES);
        // Oldest dropped: first kept line is line-500, last is line-(n-1).
        assert_eq!(got.first().unwrap(), "line-500");
        assert_eq!(got.last().unwrap(), &format!("line-{}", n - 1));
    }

    /// Write `lines` to a temp log, replay it, and return (sent, batches, done
    /// flags) as the Controller would see them after opening each frame.
    fn replay_lines(lines: &[String]) -> (usize, Vec<Vec<String>>, Vec<bool>) {
        let a = Keypair::generate();
        let b = Keypair::generate();
        let psk = b"replay-test-psk-0123456789";
        let host = Session::new(psk, &a, &b.public).unwrap();
        let ctrl = Session::new(psk, &b, &a.public).unwrap();

        let path = temp_log("replay-slim");
        let mut f = std::fs::File::create(&path).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
        drop(f);

        let (tx, mut rx) = crate::remote::outbound::channel();
        let seq = SeqCounter::default();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let sent = rt.block_on(replay_history(&host, "rm_1", "r_1", &path, &tx, &seq));
        drop(tx);
        let _ = std::fs::remove_file(&path);

        let (mut batches, mut dones) = (Vec::new(), Vec::new());
        while let Ok(env) = rx.try_recv() {
            let pt = ctrl.open(env.cipher.as_deref().unwrap()).unwrap();
            if let StreamPayload::History { lines, done, .. } = serde_json::from_slice(&pt).unwrap()
            {
                batches.push(lines);
                dones.push(done);
            }
        }
        (sent, batches, dones)
    }

    fn image_block(bytes: usize) -> serde_json::Value {
        serde_json::json!({
            "type": "image",
            "source": { "type": "base64", "media_type": "image/png", "data": "A".repeat(bytes) }
        })
    }

    #[test]
    fn replay_strips_images_nested_in_tool_results_and_user_turns() {
        let tool_result = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": [{
                "type": "tool_result",
                "tool_use_id": "tu_1",
                "content": [{ "type": "text", "text": "shot taken" }, image_block(800_000)]
            }]}
        });
        let user_turn = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": [
                { "type": "text", "text": "look" },
                image_block(500_000),
                { "type": "image", "url": format!("data:image/png;base64,{}", "B".repeat(1000)) }
            ]}
        });
        let plain = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#;
        let (sent, batches, dones) = replay_lines(&[
            tool_result.to_string(),
            user_turn.to_string(),
            plain.to_string(),
        ]);
        assert_eq!(sent, 3);
        assert_eq!(dones.last(), Some(&true));
        let got: Vec<String> = batches.concat();

        let tr: serde_json::Value = serde_json::from_str(&got[0]).unwrap();
        let inner = &tr["message"]["content"][0]["content"];
        assert_eq!(inner[0]["text"], "shot taken");
        assert_eq!(
            inner[1],
            serde_json::json!({ "type": "text", "text": REPLAY_IMAGE_PLACEHOLDER })
        );
        assert_eq!(tr["message"]["content"][0]["tool_use_id"], "tu_1");

        let ut: serde_json::Value = serde_json::from_str(&got[1]).unwrap();
        let content = ut["message"]["content"].as_array().unwrap();
        assert_eq!(content[0]["text"], "look");
        assert_eq!(content[1]["text"], REPLAY_IMAGE_PLACEHOLDER);
        assert_eq!(content[2]["text"], REPLAY_IMAGE_PLACEHOLDER);
        assert!(got[1].len() < 1000, "no base64 left");

        // Untouched lines ship byte-for-byte.
        assert_eq!(got[2], plain);
    }

    #[test]
    fn replay_truncates_long_strings_and_raw_lines() {
        let big = "x".repeat(REPLAY_MAX_STRING + 1234);
        let json_line = serde_json::json!({
            "type": "user",
            "message": { "content": [{ "type": "tool_result", "content": big }] }
        })
        .to_string();
        let raw_line = format!("not json {big}");
        let (_, batches, _) = replay_lines(&[json_line, raw_line.clone()]);
        let got = batches.concat();

        let v: serde_json::Value = serde_json::from_str(&got[0]).unwrap();
        let s = v["message"]["content"][0]["content"].as_str().unwrap();
        assert!(s.starts_with(&"x".repeat(REPLAY_MAX_STRING)));
        assert!(s.ends_with("…[truncated 1234 bytes]"), "{}", &s[s.len() - 40..]);

        let dropped = raw_line.len() - REPLAY_MAX_STRING;
        assert!(got[1].starts_with("not json "));
        assert!(got[1].ends_with(&format!("…[truncated {dropped} bytes]")));
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        // 'é' is 2 bytes, so the cap lands mid-char and must back off one byte.
        let s = format!("a{}", "é".repeat(REPLAY_MAX_STRING));
        let t = truncate_replay_str(&s).unwrap();
        let kept = t.split('…').next().unwrap();
        assert_eq!(kept.len(), REPLAY_MAX_STRING - 1);
    }

    #[test]
    fn replay_byte_budget_keeps_the_newest_lines_in_bounded_batches() {
        // 100 lines of ~40 KB each = ~4 MB: over the byte budget, under the
        // line cap, and each line is under the string cap so none are cut.
        let lines: Vec<String> = (0..100)
            .map(|i| format!("{i:03}-{}", "y".repeat(40 * 1024)))
            .collect();
        let (sent, batches, dones) = replay_lines(&lines);

        let got = batches.concat();
        assert_eq!(got.len(), sent);
        let bytes: usize = got.iter().map(String::len).sum();
        assert!(bytes <= REPLAY_MAX_BYTES, "within budget: {bytes}");
        assert!(bytes + lines[0].len() > REPLAY_MAX_BYTES, "budget used fully");
        // The tail is kept, in order.
        assert_eq!(got.last(), lines.last());
        assert_eq!(got[..], lines[lines.len() - sent..]);

        // Batches are bounded by bytes (not just 200 lines) and only the last
        // one is done.
        assert!(batches.len() > 1);
        for b in &batches {
            assert!(!b.is_empty());
            assert!(b.iter().map(String::len).sum::<usize>() <= REPLAY_BATCH_BYTES);
        }
        assert_eq!(dones.iter().filter(|d| **d).count(), 1);
        assert_eq!(dones.last(), Some(&true));
    }

    #[test]
    fn replay_missing_log_sends_empty_done() {
        let session = test_session();
        let (tx, mut rx) = crate::remote::outbound::channel();
        let seq = SeqCounter::default();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let sent = rt.block_on(replay_history(
            &session,
            "rm_1",
            "r_missing",
            std::path::Path::new("/nonexistent/does-not-exist.log"),
            &tx,
            &seq,
        ));
        drop(tx);
        assert_eq!(sent, 0);
        // Exactly one frame: an empty done=true batch.
        let env = rx.try_recv().unwrap();
        assert_eq!(env.t, FrameType::Stream);
        assert!(rx.try_recv().is_err());
    }
}
