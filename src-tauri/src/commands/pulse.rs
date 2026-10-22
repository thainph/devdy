//! Tauri commands of Project Pulse (docs/project-pulse-spec.md §8.1). Errors
//! are `"<code>: <detail>"` strings.

use crate::pulse::store::{self, ProjectPulse, Watch, WatchRequest};
use crate::pulse::PulseState;
use crate::slack_threads::CodedError;
use serde::Serialize;
use tauri::State;

type CmdResult<T> = Result<T, String>;

fn err(e: CodedError) -> String {
    e.to_string()
}

/// Watch a pasted link / reference from a button or the Pulse paste box.
#[tauri::command]
pub async fn pulse_watch(
    state: State<'_, PulseState>,
    url: String,
    project_id: String,
    via: String,
) -> CmdResult<Watch> {
    if via != "button" && via != "paste" {
        return Err(err(CodedError::new(
            "invalid_via",
            format!("unknown via: {via}"),
        )));
    }
    let db = state.db();
    let (project_id, _) = store::resolve_project(db, Some(&project_id), None)
        .await
        .map_err(err)?;
    let source = store::normalize(db, &url, &project_id).await.map_err(err)?;
    let out = store::watch(
        db,
        WatchRequest {
            project_id: &project_id,
            source: &source,
            via: &via,
            run_id: None,
            move_project: false,
        },
    )
    .await
    .map_err(err)?;
    if !out.already_watching {
        state.emit_changed(std::slice::from_ref(&out.watch.id));
        state.kick();
    }
    Ok(out.watch)
}

#[tauri::command]
pub async fn pulse_unwatch(state: State<'_, PulseState>, watch_id: String) -> CmdResult<()> {
    store::unwatch(state.db(), &watch_id).await.map_err(err)?;
    state.emit_changed(&[watch_id]);
    Ok(())
}

#[tauri::command]
pub async fn pulse_move_watch(
    state: State<'_, PulseState>,
    watch_id: String,
    project_id: String,
) -> CmdResult<Watch> {
    let db = state.db();
    let (project_id, _) = store::resolve_project(db, Some(&project_id), None)
        .await
        .map_err(err)?;
    let w = store::move_watch(db, &watch_id, &project_id)
        .await
        .map_err(err)?;
    state.emit_changed(&[watch_id]);
    state.kick();
    Ok(w)
}

/// Active watch of a source (`sourceRef` as normalised by BR-02), or null.
#[tauri::command]
pub async fn pulse_is_watched(
    state: State<'_, PulseState>,
    source_type: String,
    source_ref: String,
) -> CmdResult<Option<Watch>> {
    store::is_watched(state.db(), &source_type, &source_ref)
        .await
        .map_err(err)
}

/// Stored watches + newest digests per project; no network.
#[tauri::command]
pub async fn pulse_overview(
    state: State<'_, PulseState>,
    project_ids: Option<Vec<String>>,
) -> CmdResult<Vec<ProjectPulse>> {
    store::overview(state.db(), project_ids.as_deref())
        .await
        .map_err(err)
}

#[derive(Serialize)]
pub struct RefreshQueued {
    pub queued: usize,
}

/// Refresh the active watches of one watch / project / everything (none
/// given). Runs in the background: `pulse:refresh-progress { done, total }`
/// while it goes, `pulse:changed` per finished source.
#[tauri::command]
pub async fn pulse_refresh(
    state: State<'_, PulseState>,
    project_id: Option<String>,
    watch_id: Option<String>,
) -> CmdResult<RefreshQueued> {
    let watches = store::scope_watches(state.db(), project_id.as_deref(), watch_id.as_deref())
        .await
        .map_err(err)?;
    if let Some(id) = watch_id.as_deref() {
        if watches.is_empty() {
            return Err(err(CodedError::new(
                "not_found",
                format!("no active watch {id}"),
            )));
        }
    }
    let queued = watches.len();
    state.schedule(watches);
    Ok(RefreshQueued { queued })
}

/// The Pulse screen reports when it is open so MCP requests are polled (BR-11).
#[tauri::command]
pub async fn pulse_set_screen_open(state: State<'_, PulseState>, open: bool) -> CmdResult<()> {
    state.set_screen_open(open);
    Ok(())
}
