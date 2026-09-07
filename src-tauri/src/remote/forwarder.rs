//! Stream forwarder (FR-004/FR-006) + history replay (FR-005).
//!
//! When a room is authenticated the agent spawns [`forward_loop`], which
//! subscribes to the in-process [`RemoteBus`], seals each run event with the E2E
//! session key and pushes an envelope onto the room's outbound channel. Only the
//! single BOUND run's events are forwarded (single-session redesign); events for
//! other runs are dropped. Replay reads the persisted `.devdy/runs/<run_id>.log` and
//! sends it in bounded batches before live stream continues (FR-005/AC-07),
//! capped at [`REPLAY_MAX_LINES`].

use crate::db::Db;
use crate::remote::bus::{RemoteBus, RemoteRunEvent};
use crate::remote::protocol::{
    EngineOption, Envelope, FrameType, ModelOption, ProjectInfo, RunInfo, SlashCommandInfo,
    StreamPayload,
};
use remote_e2e::Session;
use sqlx::Row;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Cap on how many project files a `list_project_files` push returns.
const PROJECT_FILES_MAX: usize = 500;

/// Cap on how many log lines a single replay returns, to protect the link
/// (BR-006). Newer-than-cap logs are truncated from the FRONT (oldest dropped)
/// so the Controller still lands on the most recent context.
pub const REPLAY_MAX_LINES: usize = 2000;
/// Lines per replay batch envelope.
const REPLAY_BATCH: usize = 200;

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
fn seal_stream(
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
        } => StreamPayload::PermissionRequest {
            run_id,
            request_id,
            tool,
            input,
            cwd,
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
        },
    }
}

/// Forward live run events to the Controller until the room closes.
///
/// Runs until `out` is closed (room gone) or the bus lags catastrophically.
/// A `broadcast` lag (forwarder briefly slow) is tolerated: we skip the missed
/// window and keep going — the Controller can request history to backfill
/// (FR-005/FR-012). ALL runs are forwarded; access is gated at device approval,
/// not per run. When a run finishes, or the first event of a run not yet seen
/// arrives, the run list is refreshed so the Controller's browser stays current.
pub async fn forward_loop(
    bus: RemoteBus,
    session: Arc<Session>,
    room_id: String,
    out: mpsc::UnboundedSender<Envelope>,
    seq: Arc<SeqCounter>,
    db: Db,
    bound_run_id: String,
) {
    let mut rx = bus.subscribe();
    let mut done_once = false;
    loop {
        match rx.recv().await {
            Ok(ev) => {
                let run_id = ev.run_id().to_string();
                // Single-session redesign: forward ONLY the bound run's events.
                if run_id != bound_run_id {
                    continue;
                }
                let is_done = matches!(ev, RemoteRunEvent::Done { .. });
                let payload = to_payload(ev);
                if let Some(env) = seal_stream(&session, &room_id, &payload, seq.next()) {
                    if out.send(env).is_err() {
                        break; // room/writer gone
                    }
                }
                if is_done {
                    // Refresh subscription usage badges after every turn (the
                    // sidecar has captured the turn's /usage by now). Best-effort.
                    send_plan_usage(&db, &session, &room_id, &out, &seq, &bound_run_id).await;
                    // Refresh the (scoped) run list once when the bound run finishes.
                    if !done_once {
                        done_once = true;
                        send_run_list(&db, &session, &room_id, &out, &seq, &bound_run_id).await;
                    }
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(event = "remote_forward_lagged", skipped = n);
                continue;
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// Build the (scoped) run-list snapshot: just the single bound run in v1.
/// Non-secret metadata only.
pub async fn build_run_list(db: &Db, bound_run_id: &str) -> Vec<RunInfo> {
    let rows = sqlx::query(
        "SELECT r.id AS id, r.project_id AS project_id, p.name AS project_name,
                r.title AS title, r.type AS run_type, r.ref_number AS ref_number,
                r.status AS status, r.engine AS engine, r.created_at AS created_at,
                COALESCE(r.finished_at, r.started_at, r.created_at) AS updated_at
         FROM runs r JOIN projects p ON p.id = r.project_id
         WHERE r.id = ?",
    )
    .bind(bound_run_id)
    .fetch_all(db)
    .await
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
        })
        .collect()
}

/// Seal + send a fresh (scoped) run-list snapshot to the Controller.
pub async fn send_run_list(
    db: &Db,
    session: &Session,
    room_id: &str,
    out: &mpsc::UnboundedSender<Envelope>,
    seq: &SeqCounter,
    bound_run_id: &str,
) {
    let runs = build_run_list(db, bound_run_id).await;
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
    out: &mpsc::UnboundedSender<Envelope>,
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
    let payload = StreamPayload::RunMeta {
        run_id: bound_run_id.to_string(),
        engine: meta.engine,
        model: meta.model,
        permission_mode: meta.permission_mode,
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
    out: &mpsc::UnboundedSender<Envelope>,
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
pub async fn send_engine_model_options(
    session: &Session,
    room_id: &str,
    out: &mpsc::UnboundedSender<Envelope>,
    seq: &SeqCounter,
) {
    let payload = StreamPayload::EngineModelOptions {
        engines: engine_model_options(),
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
    out: &mpsc::UnboundedSender<Envelope>,
    seq: &SeqCounter,
    bound_run_id: &str,
) {
    // The controller mirrors ONE account: the one the bound run is actually
    // using (its snapshotted account, else the current default). Show that
    // account's usage + label so the controller knows which account it reflects.
    let account_id = match crate::commands::claude_accounts::run_account_id(db, bound_run_id).await {
        Some(id) => Some(id),
        None => crate::commands::claude_accounts::default_account_id(db).await,
    };
    let claude_key =
        crate::commands::stats::claude_plan_usage_key(account_id.as_deref());
    let claude = crate::commands::stats::budget_status(db, &claude_key).await;
    let codex = crate::commands::stats::budget_status(db, "plan_usage_codex").await;
    let claude_account = match account_id.as_deref() {
        Some(id) => sqlx::query_scalar::<_, String>(
            "SELECT label FROM claude_accounts WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten(),
        None => None,
    };
    let payload = StreamPayload::PlanUsage {
        claude: serde_json::to_value(&claude).ok(),
        codex: serde_json::to_value(&codex).ok(),
        claude_account,
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
    out: &mpsc::UnboundedSender<Envelope>,
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
    let rows = sqlx::query("SELECT id, name, path FROM projects ORDER BY name")
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
    out: &mpsc::UnboundedSender<Envelope>,
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

/// Send the persisted log for `run_id` back to the Controller in batches
/// (FR-005/AC-07). Returns the number of lines sent. The final batch carries
/// `done = true`; live stream then continues without overlap because the
/// Controller only started receiving live events after it joined.
pub async fn replay_history(
    session: &Session,
    room_id: &str,
    run_id: &str,
    log_path: &std::path::Path,
    out: &mpsc::UnboundedSender<Envelope>,
    seq: &SeqCounter,
) -> usize {
    let content = std::fs::read_to_string(log_path).unwrap_or_default();
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    // BR-006: cap the replay; keep the most recent lines.
    if lines.len() > REPLAY_MAX_LINES {
        let drop = lines.len() - REPLAY_MAX_LINES;
        lines.drain(0..drop);
    }
    let total = lines.len();

    let mut chunks = lines.chunks(REPLAY_BATCH).peekable();
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
            lines: chunk.to_vec(),
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

        let (tx, mut rx) = mpsc::unbounded_channel();
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

    #[test]
    fn replay_missing_log_sends_empty_done() {
        let session = test_session();
        let (tx, mut rx) = mpsc::unbounded_channel();
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
