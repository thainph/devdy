//! Quitting with work still running.
//!
//! Every quit path (Cmd+Q, closing the main window, tray Quit) goes through
//! [`request_quit`]. With nothing but app plumbing running it quits at once.
//! Otherwise, depending on the `quit_background_policy` setting, it stops
//! everything, keeps the leftovers, or asks the main window
//! (`app:quit-requested`), which answers through [`resolve_quit`]. Asking again
//! while the question is open force-quits, so a hung webview can never trap
//! the user.

use super::commands::{snapshot, BgEntry};
use super::{ps, terminate_blocking, Counts, ProcState, ProcTracker};
use crate::db::Db;
use crate::runs::sidecar::kill_process_group;
use crate::runs::RunRegistry;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};

pub const QUIT_REQUESTED_EVENT: &str = "app:quit-requested";
pub const POLICY_KEY: &str = "quit_background_policy";
/// Grace between SIGTERM and SIGKILL when quitting (the app is waiting).
const QUIT_GRACE: Duration = Duration::from_millis(1500);
/// How long an unanswered question counts as open. A quit request inside this
/// window force-quits; after it (e.g. the webview reloaded and lost the
/// dialog) the question is asked again instead.
const PENDING_TTL: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct QuitState {
    /// When the main window was asked, if it has not answered yet.
    pending: Mutex<Option<Instant>>,
    /// Cleanup done; the next exit request goes through.
    confirmed: AtomicBool,
}

impl QuitState {
    fn is_pending(&self) -> bool {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some_and(|at| at.elapsed() < PENDING_TTL)
    }

    fn set_pending(&self, on: bool) {
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = on.then(Instant::now);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitAction {
    /// Stop every tracked process.
    StopAll,
    /// Stop runs and app plumbing; leave leftover / orphan processes running
    /// (they are listed as orphans at the next launch).
    KeepLeftovers,
}

impl QuitAction {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "stop_all" => Some(Self::StopAll),
            "keep_leftovers" => Some(Self::KeepLeftovers),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct QuitRequest {
    counts: Counts,
    entries: Vec<BgEntry>,
}

/// Decide whether the app may exit now. `true`: cleanup is done, let the exit
/// (or window close) proceed. `false`: the caller must prevent it; the main
/// window was asked and will answer via [`resolve_quit`].
pub fn request_quit(app: &AppHandle) -> bool {
    let qs = app.state::<QuitState>();
    if qs.confirmed.load(Ordering::SeqCst) {
        return true;
    }
    if qs.is_pending() {
        // Second quit while the question is open: stop everything and go.
        finish(app, QuitAction::StopAll);
        return true;
    }
    let tracker = app.state::<ProcTracker>().inner().clone();
    let db = app.state::<Db>().inner().clone();
    let registry = app.state::<RunRegistry>().inner().clone();
    let (snap, live_runs, policy) = tauri::async_runtime::block_on(async {
        let snap = snapshot(app, &tracker, &db).await;
        let live_runs = registry.lock().await.len();
        let policy: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
            .bind(POLICY_KEY)
            .fetch_optional(&db)
            .await
            .ok()
            .flatten();
        (snap, live_runs, policy)
    });
    // Only user work warrants a question: live runs, and whatever runs (or a
    // previous session) left behind. App plumbing is always stopped silently.
    let entries: Vec<BgEntry> = snap.entries.into_iter().filter(|e| !e.infra).collect();
    let count = |state: ProcState| entries.iter().filter(|e| e.state == state).count();
    let counts = Counts {
        active: count(ProcState::Active).max(live_runs),
        leftover: count(ProcState::Leftover),
        orphan: count(ProcState::Orphan),
        total_processes: entries.iter().map(|e| e.processes.len()).sum(),
    };
    if counts.active == 0 && counts.leftover == 0 && counts.orphan == 0 {
        finish(app, QuitAction::KeepLeftovers);
        return true;
    }
    if let Some(action) = policy.as_deref().and_then(QuitAction::parse) {
        finish(app, action);
        return true;
    }
    let Some(main) = app.get_webview_window("main") else {
        finish(app, QuitAction::StopAll);
        return true;
    };
    qs.set_pending(true);
    let _ = main.unminimize();
    let _ = main.show();
    let _ = main.set_focus();
    if main
        .emit(QUIT_REQUESTED_EVENT, QuitRequest { counts, entries })
        .is_err()
    {
        qs.set_pending(false);
        finish(app, QuitAction::StopAll);
        return true;
    }
    false
}

/// The main window's answer to [`QUIT_REQUESTED_EVENT`].
#[tauri::command]
pub async fn resolve_quit(
    app: AppHandle,
    db: State<'_, Db>,
    action: String,
    remember: Option<bool>,
) -> Result<(), String> {
    let qs = app.state::<QuitState>();
    if action == "cancel" {
        qs.set_pending(false);
        return Ok(());
    }
    let parsed =
        QuitAction::parse(&action).ok_or_else(|| format!("Unknown quit action '{action}'"))?;
    if remember.unwrap_or(false) {
        sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)")
            .bind(POLICY_KEY)
            .bind(&action)
            .execute(db.inner())
            .await
            .map_err(|e| e.to_string())?;
    }
    let app2 = app.clone();
    tokio::task::spawn_blocking(move || finish(&app2, parsed))
        .await
        .map_err(|e| e.to_string())?;
    for (label, win) in app.webview_windows() {
        if label != "main" {
            let _ = win.close();
        }
    }
    app.exit(0);
    Ok(())
}

/// Run the cleanup for `action` (once) and mark the quit confirmed.
fn finish(app: &AppHandle, action: QuitAction) {
    let qs = app.state::<QuitState>();
    if qs.confirmed.swap(true, Ordering::SeqCst) {
        return;
    }
    qs.set_pending(false);

    let tracker = app.state::<ProcTracker>().inner().clone();
    if let Some(rows) = ps::snapshot() {
        tracker.apply_sample(&rows);
    }
    // Decide before the sidecars are stopped: stopping releases the active
    // entries, which would make them look like leftovers.
    let targets: Vec<_> = tracker
        .targets(None)
        .into_iter()
        .filter(|t| action == QuitAction::StopAll || t.state == ProcState::Active)
        .collect();

    stop_app_sidecars(app);

    let mut pids = Vec::new();
    let mut groups = Vec::new();
    for t in targets {
        pids.extend(t.pids);
        groups.extend(t.groups);
    }
    let n = terminate_blocking(&pids, &groups, false, QUIT_GRACE);
    tracing::info!(event = "quit_cleanup", action = ?action, processes = n);
    if let Some(rows) = ps::snapshot() {
        tracker.apply_sample(&rows);
    }
    tracker.persist_now();
}

/// `RunEvent::Exit`. The question could not be asked when the exit did not go
/// through [`request_quit`] — macOS Dock "Quit", logout / shutdown (they send
/// `terminate:`, which cannot be prevented). Apply the saved policy, stopping
/// everything when it is "ask": nobody is there to answer.
pub fn on_exit(app: &AppHandle) {
    if app.state::<QuitState>().confirmed.load(Ordering::SeqCst) {
        stop_app_sidecars(app);
        return;
    }
    let db = app.state::<Db>().inner().clone();
    let policy: Option<String> = tauri::async_runtime::block_on(async move {
        sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
            .bind(POLICY_KEY)
            .fetch_optional(&db)
            .await
            .ok()
            .flatten()
    });
    let action = policy
        .as_deref()
        .and_then(QuitAction::parse)
        .unwrap_or(QuitAction::StopAll);
    finish(app, action);
}

/// Kill every registered run sidecar (and its CLI) and the warm translate /
/// Slack / Pulse / mascot sessions, so no `claude` / `codex` process lingers
/// burning API tokens. Idempotent; also run on `RunEvent::Exit`.
pub fn stop_app_sidecars(app: &AppHandle) {
    let registry = app.state::<RunRegistry>().inner().clone();
    tauri::async_runtime::block_on(async move {
        let mut reg = registry.lock().await;
        for (_, handles) in reg.iter_mut() {
            if let Some(pgid) = handles.pgid {
                kill_process_group(pgid);
            }
            let _ = handles.child.start_kill();
        }
        reg.clear();
    });
    let translate = app
        .state::<crate::commands::translate::TranslateState>()
        .inner()
        .clone();
    let slack = app
        .state::<crate::slack_threads::SlackTranslateState>()
        .inner()
        .clone();
    let pulse = app.state::<crate::pulse::PulseState>().inner().clone();
    let mascot = app
        .state::<crate::commands::mascot_speak::MascotSpeakState>()
        .inner()
        .clone();
    tauri::async_runtime::block_on(async move {
        translate.shutdown().await;
        slack.0.shutdown().await;
        pulse.shutdown().await;
        mascot.shutdown().await;
    });
}
