//! Live mirroring of the shared Claude/Codex transcript stores into Devdy.
//!
//! Both engines persist conversations to a store their CLIs / VS Code extensions
//! also write — Claude at `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`,
//! Codex at `~/.codex/sessions/<date>/rollout-<ts>-<thread-id>.jsonl`. This
//! watcher tails both so anything done OUTSIDE Devdy (a new session, or more
//! turns added to one Devdy started) shows up without a manual refresh.
//!
//! On a debounced change it maps the file back to a project and upserts the
//! session into Devdy's run/log/usage model, then emits `sessions:changed` (with
//! the legacy `claude:sessions_changed` alias) so the UI refetches the run list
//! and reloads the open run. Live runs are left untouched — the upsert skips
//! sessions owned by a still-running Devdy run.

use crate::commands::codex_sessions::{rollout_meta, upsert_codex_session_file};
use crate::commands::sessions::{encode_project_dir, upsert_claude_session, SyncOutcome};
use sqlx::{Pool, Sqlite};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
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
    let roots = [
        home.join(".claude").join("projects"),
        home.join(".codex").join("sessions"),
    ];
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
                let db = db.clone();
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    handle_change(&db, &app, &path).await;
                });
            }
        }
    });
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
        // Codex: the project is recorded as `cwd` inside the rollout. We already
        // hold the file path, so pass it through (no tree walk to locate it).
        let Some((cwd, thread_id)) = rollout_meta(path) else {
            return;
        };
        upsert_for_project(db, app, ProjectMatch::Cwd(&cwd), &thread_id, EngineCall::Codex(path)).await;
        // Plan rate-limits are account-global (not per-project): refresh the Codex
        // budget snapshot whenever any rollout gains new usage data, so the badge
        // stays current during live runs and from external codex CLI use.
        if let Some(rl) = crate::commands::codex_sessions::latest_codex_rate_limits(path) {
            if crate::commands::codex_sessions::persist_codex_plan_usage(db, &rl).await {
                let _ = app.emit("plan_usage_updated", serde_json::json!({ "provider": "codex" }));
            }
        }
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
        upsert_for_project(db, app, ProjectMatch::EncodedDir(dir_name), session_id, EngineCall::Claude).await;
    }
}

enum ProjectMatch<'a> {
    /// Match a project whose `encode_project_dir(path)` equals this dir name.
    EncodedDir(&'a str),
    /// Match a project whose `path` equals this cwd.
    Cwd(&'a str),
}

enum EngineCall<'a> {
    Claude,
    /// Carries the already-known rollout path so the upsert skips the tree walk.
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

    let Ok(projects) = sqlx::query("SELECT id, name, path FROM projects")
        .fetch_all(db)
        .await
    else {
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
            EngineCall::Claude => {
                upsert_claude_session(db, &project_id, &project_name, &path, session_id, true).await
            }
        };
        if let Ok(SyncOutcome::Imported(run_id)) | Ok(SyncOutcome::Updated(run_id)) = outcome {
            let payload = serde_json::json!({
                "project_id": project_id,
                "session_id": session_id,
                "run_id": run_id,
            });
            // Engine-neutral name (used for both Claude and Codex); the old
            // `claude:` name is kept as an alias for any older listener.
            let _ = app.emit("sessions:changed", &payload);
            let _ = app.emit("claude:sessions_changed", &payload);
        }
        return; // a project is uniquely identified
    }
}
