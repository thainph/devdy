//! Live run / project lists: re-push them to a Controller whenever the desktop
//! changes what they show or their order (pin, rename, delete, project
//! reorder …), so the phone's run browser never lags behind the desktop.
//!
//! Same shape as the live Focus board ([`crate::remote::focus::start_live_board`]):
//! one app-wide listener per session, bursts coalesced, the task holding only a
//! weak reference so it goes away with the session.

use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Listener};

use crate::remote::forwarder::{send_project_list, send_run_list};
use crate::remote::handler::HandlerCtx;

/// A run was added, removed, renamed or (un)pinned on the Host.
pub const RUNS_CHANGED_EVENT: &str = "runs:list_changed";
/// A project was added, removed, renamed or reordered on the Host.
pub const PROJECTS_CHANGED_EVENT: &str = "projects:list_changed";

/// Coalesces a burst (e.g. "Clear all" deleting many runs) into one push.
const LIVE_DEBOUNCE: Duration = Duration::from_millis(300);

/// Tell every Remote Control session the run list changed.
pub fn emit_runs_changed(app: &AppHandle) {
    let _ = app.emit(RUNS_CHANGED_EVENT, ());
}

/// What a burst of change events owes the Controller.
#[derive(Default, Clone, Copy)]
struct Due {
    runs: bool,
    projects: bool,
}

/// The listeners of one session and the task that turns them into pushes.
/// Dropped with the session's [`HandlerCtx`].
pub struct LiveLists {
    app: AppHandle,
    listeners: [tauri::EventId; 2],
    task: tokio::task::AbortHandle,
}

impl Drop for LiveLists {
    fn drop(&mut self) {
        for listener in self.listeners {
            self.app.unlisten(listener);
        }
        self.task.abort();
    }
}

pub fn start_live_lists(ctx: &Arc<HandlerCtx>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Due>();
    let runs_tx = tx.clone();
    let runs = ctx.app.listen_any(RUNS_CHANGED_EVENT, move |_| {
        let _ = runs_tx.send(Due { runs: true, projects: false });
    });
    let projects = ctx.app.listen_any(PROJECTS_CHANGED_EVENT, move |_| {
        // Runs carry their project's name, so a rename re-sends them too.
        let _ = tx.send(Due { runs: true, projects: true });
    });
    let weak = Arc::downgrade(ctx);
    let task = tokio::spawn(async move {
        while let Some(first) = rx.recv().await {
            tokio::time::sleep(LIVE_DEBOUNCE).await;
            let mut due = first;
            while let Ok(more) = rx.try_recv() {
                due.runs |= more.runs;
                due.projects |= more.projects;
            }
            let Some(ctx) = weak.upgrade() else { break };
            if due.projects {
                send_project_list(&ctx.db, &ctx.session, &ctx.room_id, &ctx.out, &ctx.seq).await;
            }
            if due.runs {
                send_run_list(
                    &ctx.db,
                    &ctx.session,
                    &ctx.room_id,
                    &ctx.out,
                    &ctx.seq,
                    ctx.list_filter(),
                )
                .await;
            }
        }
    });
    let _ = ctx.live_lists.set(LiveLists {
        app: ctx.app.clone(),
        listeners: [runs, projects],
        task: task.abort_handle(),
    });
}
