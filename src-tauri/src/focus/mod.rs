//! Focus (docs/focus-board-spec.md): the global todo board.
//!
//! - `score`: work-hours arithmetic, buckets, score + reasons, start_by,
//!   calibration (pure, BR-03..06, BR-10)
//! - `source`: todo source link normalisation (pure, BR-02)
//! - `validate`: todo field checks (pure, BR-01)
//! - `settings`: `focus.*` rows of the settings table (§5.2)
//! - `scheduler`: reminder decisions (pure) + the 60 s tick (§3.4, BR-07)
//! - `capacity` (pure, BR-08) / `calendar` (Google fetch + cache, §3.5)
//! - `source_content`, `github`, `ai_util`: kept from Project Pulse for the AI
//!   commands of phase P3
//! - `ai`: the AI prompts, answer validation, free slots and BR-11 (pure, P3)

pub mod ai;
pub mod ai_util;
pub mod calendar;
pub mod capacity;
pub mod github;
pub mod scheduler;
pub mod score;
pub mod settings;
pub mod source;
pub mod source_content;
pub mod validate;

use crate::db::Db;
use crate::item_links::{self, ItemKind};
use tauri::{AppHandle, Emitter};

/// Broadcast after every todo write (with `item:changed`).
pub const CHANGED_EVENT: &str = "focus:changed";

/// Tell the Focus board and the item lists that todos changed.
pub fn emit_todos_changed(app: &AppHandle) {
    let _ = app.emit(CHANGED_EVENT, ());
    item_links::emit_changed(app, Some(ItemKind::Todo));
}

/// A session was started to work on `todo_id`: link it (`created_from_item`) and
/// move the todo to `doing`. Bookkeeping only — failures are logged, not raised,
/// since the session is already running.
pub async fn start_todo_for_run(app: &AppHandle, db: &Db, todo_id: &str, run_id: &str) {
    if let Err(e) = item_links::link(db, ItemKind::Todo, todo_id, run_id, "created_from_item").await {
        tracing::warn!(event = "todo_run_link_failed", run_id, error = %String::from(e));
    }
    let now = chrono::Utc::now().to_rfc3339();
    if let Err(e) = sqlx::query(
        "UPDATE todos SET status = 'doing', started_at = COALESCE(started_at, ?), updated_at = ? \
         WHERE id = ? AND done = 0",
    )
    .bind(&now)
    .bind(&now)
    .bind(todo_id)
    .execute(db)
    .await
    {
        tracing::warn!(event = "todo_start_status_failed", run_id, error = %e);
    }
    emit_todos_changed(app);
}

/// A session started by [`start_todo_for_run`] never ran: drop the link and put
/// the todo back to `todo` (unless something else moved it meanwhile).
pub async fn undo_todo_start(app: &AppHandle, db: &Db, todo_id: &str, run_id: &str) {
    let _ = sqlx::query("DELETE FROM todo_runs WHERE todo_id = ? AND run_id = ?")
        .bind(todo_id)
        .bind(run_id)
        .execute(db)
        .await;
    let _ = sqlx::query("UPDATE todos SET status = 'todo', updated_at = ? WHERE id = ? AND done = 0 AND status = 'doing'")
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(todo_id)
        .execute(db)
        .await;
    emit_todos_changed(app);
}

/// Keep a todo's status in step with the session working on it (linked as
/// `created_from_item`): a turn starting → `doing`; a turn ending → `waiting`
/// (its result — or its failure — waits for the user). Done todos and todos the
/// user moved elsewhere are left alone.
pub async fn sync_todo_with_run(app: &AppHandle, db: &Db, run_id: &str, turn_running: bool) {
    let (to, from) = if turn_running { ("doing", "waiting") } else { ("waiting", "doing") };
    let changed = sqlx::query(
        "UPDATE todos SET status = ?, updated_at = ? \
         WHERE done = 0 AND status = ? AND id IN \
           (SELECT todo_id FROM todo_runs WHERE run_id = ? AND source = 'created_from_item')",
    )
    .bind(to)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(from)
    .bind(run_id)
    .execute(db)
    .await
    .map(|r| r.rows_affected())
    .unwrap_or(0);
    if changed > 0 {
        emit_todos_changed(app);
    }
}
