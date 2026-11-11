//! Live mirroring of the shared Claude/Codex transcript stores into Devdy.
//!
//! Both engines persist conversations to a store their CLIs / VS Code extensions
//! also write — Claude at `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`,
//! Codex at `~/.codex/sessions/<date>/rollout-<ts>-<thread-id>.jsonl`. This
//! watcher tails both so anything done OUTSIDE Devdy (a new session, or more
//! turns added to one Devdy started) shows up without a manual refresh.
//!
//! On a debounced change it maps the file back to a project and upserts the
//! session into Devdy's run/log/usage model, then emits `sessions:changed` so
//! the UI refetches the run list and reloads the open run. Live runs are left
//! untouched: Devdy's own running sessions write to these stores too (so they
//! fire constantly), and are skipped by one cheap query before any file or
//! project work.

use crate::commands::codex_sessions::{
    rollout_meta, tail_rate_limits, thread_id_from_rollout_name, upsert_codex_session_file,
    RateLimitTail,
};
use crate::commands::sessions::{blocking, encode_project_dir, upsert_claude_session_file, SyncOutcome};
use sqlx::{Pool, Sqlite};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

/// Coalesce the burst of events the engines emit while appending to a transcript.
const DEBOUNCE: Duration = Duration::from_millis(400);

/// Start watching the Claude and Codex transcript stores for changes. Spawns a
/// background thread that owns the OS watcher and a batching loop; returns
/// immediately. A failure to set up the watcher is logged and otherwise ignored
/// (the on-focus / project-open reconcile paths still keep Devdy in sync).
pub fn start(db: Pool<Sqlite>, app: AppHandle) {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let home = PathBuf::from(home);
    let mut roots = vec![
        home.join(".claude").join("projects"),
        home.join(".codex").join("sessions"),
    ];
    // Also watch every managed Claude account's transcript store, so external
    // CLI/VS Code sessions run under a multi-account profile mirror in live too
    // (AC-011). Accounts added later are still picked up by the on-open reconcile.
    for cfg in
        tauri::async_runtime::block_on(crate::commands::claude_accounts::account_config_dirs(&db))
    {
        roots.push(PathBuf::from(cfg).join("projects"));
    }
    if roots.iter().all(|r| !r.is_dir()) {
        return;
    }

    std::thread::spawn(move || {
        use notify::{RecursiveMode, Watcher};

        let (tx, rx) = std::sync::mpsc::channel();
        let mut watcher = match notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        }) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("[session_watcher] failed to create watcher: {e}");
                return;
            }
        };
        for root in &roots {
            if root.is_dir() {
                if let Err(e) = watcher.watch(root, RecursiveMode::Recursive) {
                    eprintln!("[session_watcher] failed to watch {}: {e}", root.display());
                }
            }
        }

        // Keep the watcher alive for the lifetime of this thread.
        let _watcher = watcher;
        let in_flight: InFlight = Arc::default();

        loop {
            // Block until something changes, then drain a short debounce window
            // so a flurry of line-appends becomes one reconcile per file.
            let first = match rx.recv() {
                Ok(ev) => ev,
                Err(_) => break, // sender dropped — app shutting down
            };
            let mut files: HashSet<PathBuf> = HashSet::new();
            collect(first, &mut files);
            let deadline = std::time::Instant::now() + DEBOUNCE;
            while let Ok(ev) =
                rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            {
                collect(ev, &mut files);
            }

            for path in files {
                // At most one pass per file at a time: a change landing while a
                // pass runs only marks it dirty, and that pass reruns once.
                {
                    let mut map = in_flight.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(dirty) = map.get_mut(&path) {
                        *dirty = true;
                        continue;
                    }
                    map.insert(path.clone(), false);
                }
                let db = db.clone();
                let app = app.clone();
                let in_flight = in_flight.clone();
                tauri::async_runtime::spawn(async move {
                    let mut guard = ReleaseOnPanic {
                        in_flight: &in_flight,
                        path: &path,
                        armed: true,
                    };
                    loop {
                        handle_change(&db, &app, &path).await;
                        if !take_rerun(&in_flight, &path) {
                            break;
                        }
                    }
                    guard.armed = false;
                });
            }
        }
    });
}

/// Paths with a pass in flight → whether another change arrived during it.
type InFlight = Arc<Mutex<HashMap<PathBuf, bool>>>;

/// End of a pass over `path`: if a change arrived meanwhile, clear the flag and
/// return true (run again); otherwise release the path.
fn take_rerun(in_flight: &InFlight, path: &Path) -> bool {
    let mut map = in_flight.lock().unwrap_or_else(|e| e.into_inner());
    match map.get_mut(path) {
        Some(dirty) if *dirty => {
            *dirty = false;
            true
        }
        _ => {
            map.remove(path);
            false
        }
    }
}

/// Releases `path` if its pass panics, so later changes aren't parked behind a
/// dead pass forever. Disarmed on normal exit, where `take_rerun` released it.
struct ReleaseOnPanic<'a> {
    in_flight: &'a InFlight,
    path: &'a Path,
    armed: bool,
}

impl Drop for ReleaseOnPanic<'_> {
    fn drop(&mut self) {
        if self.armed {
            let mut map = self.in_flight.lock().unwrap_or_else(|e| e.into_inner());
            map.remove(self.path);
        }
    }
}

/// Collect touched `*.jsonl` transcript paths from a watcher event.
fn collect(res: notify::Result<notify::Event>, out: &mut HashSet<PathBuf>) {
    let Ok(event) = res else { return };
    for path in event.paths {
        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.insert(path);
        }
    }
}

/// Map a changed transcript back to its project and mirror the session in,
/// dispatching on which store the file lives in.
async fn handle_change(db: &Pool<Sqlite>, app: &AppHandle, path: &Path) {
    let is_codex = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("rollout-"));

    if is_codex {
        // Plan rate-limits are account-global (not per-project): refresh the Codex
        // budget snapshot whenever any rollout gains new usage data, so the badge
        // stays current during live runs and from external codex CLI use. Done
        // before the live-run skip below — live runs are the main source.
        if let Some(rl) = tail_codex_rate_limits(path).await {
            if crate::commands::codex_sessions::persist_codex_plan_usage(db, &rl).await {
                let _ = app.emit("plan_usage_updated", serde_json::json!({ "provider": "codex" }));
            }
        }
        // The thread id is in the file name; skip a live run's rollout before
        // even opening it.
        if let Some(id) = thread_id_from_rollout_name(path) {
            if owned_by_running_run(db, &id).await {
                return;
            }
        }
        // Codex: the project is recorded as `cwd` inside the rollout. We already
        // hold the file path, so pass it through (no tree walk to locate it).
        let meta = {
            let path = path.to_path_buf();
            blocking(move || rollout_meta(&path)).await
        };
        let Ok(Some((cwd, thread_id))) = meta else {
            return;
        };
        upsert_for_project(db, app, ProjectMatch::Cwd(&cwd), &thread_id, EngineCall::Codex(path)).await;
    } else {
        // Claude: the parent directory name is the encoded project path.
        let Some(dir_name) = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
        else {
            return;
        };
        let Some(session_id) = path.file_stem().and_then(|s| s.to_str()) else {
            return;
        };
        if owned_by_running_run(db, session_id).await {
            return;
        }
        upsert_for_project(db, app, ProjectMatch::EncodedDir(dir_name), session_id, EngineCall::Claude(path)).await;
    }
}

/// Whether a still-running Devdy run owns this session. Such a run streams its
/// own updates and the upsert would skip it anyway — this answers that up front
/// with one query, instead of after the project scan, account-dir lookups and
/// tombstone/run queries the full upsert does on every ~400ms transcript write.
async fn owned_by_running_run(db: &Pool<Sqlite>, session_id: &str) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT EXISTS(SELECT 1 FROM runs WHERE session_id = ? AND status = 'running')",
    )
    .bind(session_id)
    .fetch_one(db)
    .await
    .is_ok_and(|n| n > 0)
}

/// Rate-limit read positions per rollout path (see [`tail_rate_limits`]).
static RATE_TAILS: OnceLock<Mutex<HashMap<PathBuf, RateLimitTail>>> = OnceLock::new();

/// Bound on remembered rollouts; past it the map is reset, which only costs one
/// full re-read of each rollout that changes afterwards.
const MAX_RATE_TAILS: usize = 512;

/// Newest rate-limits appended to `path` since its last pass, read on the
/// blocking pool. The entry is taken out of the map for the read (no lock held
/// across I/O); the per-path in-flight guard means no other pass races on it.
async fn tail_codex_rate_limits(path: &Path) -> Option<serde_json::Value> {
    let tails = RATE_TAILS.get_or_init(Mutex::default);
    let mut state = tails
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(path)
        .unwrap_or_default();
    let path = path.to_path_buf();
    let (path, state, rl) = blocking(move || {
        let rl = tail_rate_limits(&mut state, &path);
        (path, state, rl)
    })
    .await
    .ok()?;
    let mut map = tails.lock().unwrap_or_else(|e| e.into_inner());
    if map.len() >= MAX_RATE_TAILS {
        map.clear();
    }
    if path.exists() {
        map.insert(path, state);
    }
    rl
}

enum ProjectMatch<'a> {
    /// Match a project whose `encode_project_dir(path)` equals this dir name.
    EncodedDir(&'a str),
    /// Match a project whose `path` equals this cwd.
    Cwd(&'a str),
}

/// Each carries the already-known transcript path so the upsert skips locating
/// it (the Codex tree walk / the Claude account-dir lookup).
enum EngineCall<'a> {
    Claude(&'a Path),
    Codex(&'a Path),
}

/// Find the project a transcript belongs to and upsert the session, emitting on
/// a real change.
async fn upsert_for_project(
    db: &Pool<Sqlite>,
    app: &AppHandle,
    matcher: ProjectMatch<'_>,
    session_id: &str,
    engine: EngineCall<'_>,
) {
    use sqlx::Row;

    // A Codex cwd is matched exactly, so let SQLite filter; a Claude encoded dir
    // name can only be compared after encoding each project path.
    let projects = match matcher {
        ProjectMatch::Cwd(cwd) => {
            sqlx::query("SELECT id, name, path FROM projects WHERE path = ?")
                .bind(cwd)
                .fetch_all(db)
                .await
        }
        ProjectMatch::EncodedDir(_) => {
            sqlx::query("SELECT id, name, path FROM projects").fetch_all(db).await
        }
    };
    let Ok(projects) = projects else {
        return;
    };

    for row in projects {
        let path: String = row.get("path");
        let matched = match matcher {
            ProjectMatch::EncodedDir(dir) => encode_project_dir(&path) == dir,
            ProjectMatch::Cwd(cwd) => path == cwd,
        };
        if !matched {
            continue;
        }
        let project_id: String = row.get("id");
        let project_name: String = row.get("name");
        let outcome = match engine {
            EngineCall::Codex(file) => {
                upsert_codex_session_file(db, &project_id, &project_name, &path, session_id, file, true).await
            }
            EngineCall::Claude(file) => {
                upsert_claude_session_file(db, &project_id, &project_name, &path, session_id, file, true)
                    .await
            }
        };
        if let Ok(SyncOutcome::Imported(run_id)) | Ok(SyncOutcome::Updated(run_id)) = outcome {
            let payload = serde_json::json!({
                "project_id": project_id,
                "session_id": session_id,
                "run_id": run_id,
            });
            // Engine-neutral name (used for both Claude and Codex).
            let _ = app.emit("sessions:changed", &payload);
        }
        return; // a project is uniquely identified
    }
}
