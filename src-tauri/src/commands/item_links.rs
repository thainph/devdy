//! Tauri commands linking notes / todos to sessions (runs) — the sibling of
//! the Slack thread link commands. Errors are `"<code>: <detail>"` strings.
//! Writes broadcast `item:changed` so the list windows resync.

use crate::db::Db;
use crate::item_links::{self, ItemKind, ItemRunLink, RunLinkedItem};
use crate::slack_threads::run_links::LinkRunResult;
use tauri::{AppHandle, State};

type CmdResult<T> = Result<T, String>;

/// Link a note / todo to a session (idempotent). An item without a project
/// takes the run's.
#[tauri::command]
pub async fn link_item_run(
    app: AppHandle,
    db: State<'_, Db>,
    kind: ItemKind,
    item_id: String,
    run_id: String,
    source: String,
) -> CmdResult<LinkRunResult> {
    let r = item_links::link(db.inner(), kind, &item_id, &run_id, &source)
        .await
        .map_err(|e| e.to_string())?;
    if r.created || r.project_set {
        item_links::emit_changed(&app, Some(kind));
    }
    Ok(r)
}

#[tauri::command]
pub async fn unlink_item_run(
    app: AppHandle,
    db: State<'_, Db>,
    kind: ItemKind,
    item_id: String,
    run_id: String,
) -> CmdResult<()> {
    if item_links::unlink(db.inner(), kind, &item_id, &run_id)
        .await
        .map_err(|e| e.to_string())?
    {
        item_links::emit_changed(&app, Some(kind));
    }
    Ok(())
}

/// Sessions linked to a note / todo (runs that still exist).
#[tauri::command]
pub async fn list_item_runs(
    db: State<'_, Db>,
    kind: ItemKind,
    item_id: String,
) -> CmdResult<Vec<ItemRunLink>> {
    item_links::runs_of_item(db.inner(), kind, &item_id)
        .await
        .map_err(|e| e.to_string())
}

/// Notes / todos linked to a session.
#[tauri::command]
pub async fn list_run_items(db: State<'_, Db>, run_id: String) -> CmdResult<Vec<RunLinkedItem>> {
    item_links::items_of_run(db.inner(), &run_id)
        .await
        .map_err(|e| e.to_string())
}
