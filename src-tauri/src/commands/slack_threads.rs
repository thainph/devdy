//! Tauri commands of the Slack thread window (SRS §10.2). Errors are
//! `"<code>: <detail>"` strings.

use crate::slack_threads::run_links::{self, LinkRunResult, SlackThreadRunLink};
use crate::slack_threads::store::{
    self, AddSlackThreadResult, SlackPromptText, SlackThreadSummary, SlackThreadView,
};
use crate::slack_threads::token::{self, SlackTokenStatus};
use crate::slack_threads::{CodedError, SlackThreadsState};
use tauri::State;

type CmdResult<T> = Result<T, String>;

fn err(e: CodedError) -> String {
    e.to_string()
}

/// Add a thread by link (or re-import it when it already exists).
#[tauri::command]
pub async fn add_slack_thread(
    state: State<'_, SlackThreadsState>,
    url: String,
    project_id: Option<String>,
) -> CmdResult<AddSlackThreadResult> {
    state
        .add_thread(&url, project_id.as_deref(), None, false)
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn list_slack_threads(
    state: State<'_, SlackThreadsState>,
    project_id: Option<String>,
    query: Option<String>,
) -> CmdResult<Vec<SlackThreadSummary>> {
    store::list(&state.ctx().db, project_id.as_deref(), query.as_deref())
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn get_slack_thread(
    state: State<'_, SlackThreadsState>,
    id: String,
) -> CmdResult<SlackThreadView> {
    let ctx = state.ctx();
    store::get_view(&ctx.db, &ctx.app_data, &id)
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn refresh_slack_thread(
    state: State<'_, SlackThreadsState>,
    id: String,
) -> CmdResult<SlackThreadSummary> {
    state.refresh(&id).await.map_err(err)
}

#[tauri::command]
pub async fn mark_slack_thread_seen(
    state: State<'_, SlackThreadsState>,
    id: String,
) -> CmdResult<()> {
    store::mark_seen(&state.ctx().db, &id).await.map_err(err)?;
    state.ctx().emit_changed(&id, "updated");
    Ok(())
}

#[tauri::command]
pub async fn rename_slack_thread(
    state: State<'_, SlackThreadsState>,
    id: String,
    title: String,
) -> CmdResult<SlackThreadSummary> {
    let s = store::rename(&state.ctx().db, &id, &title)
        .await
        .map_err(err)?;
    state.ctx().emit_changed(&id, "updated");
    Ok(s)
}

#[tauri::command]
pub async fn set_slack_thread_project(
    state: State<'_, SlackThreadsState>,
    id: String,
    project_id: Option<String>,
) -> CmdResult<()> {
    store::set_project(&state.ctx().db, &id, project_id.as_deref())
        .await
        .map_err(err)?;
    state.ctx().emit_changed(&id, "updated");
    Ok(())
}

#[tauri::command]
pub async fn reorder_slack_threads(
    state: State<'_, SlackThreadsState>,
    ids: Vec<String>,
) -> CmdResult<()> {
    store::reorder(&state.ctx().db, &ids).await.map_err(err)?;
    for id in &ids {
        state.ctx().emit_changed(id, "updated");
    }
    Ok(())
}

/// Delete threads: messages, files, rows and `slack-threads/<id>/`.
#[tauri::command]
pub async fn delete_slack_threads(
    state: State<'_, SlackThreadsState>,
    ids: Vec<String>,
) -> CmdResult<()> {
    state.delete(&ids).await.map_err(err)
}

/// Which MCP servers provide a Slack token (no network call).
#[tauri::command]
pub async fn get_slack_token_status(
    state: State<'_, SlackThreadsState>,
) -> CmdResult<SlackTokenStatus> {
    let ctx = state.ctx();
    token::token_status(&ctx.db, &ctx.lookup)
        .await
        .map_err(|e| err(store::internal(e)))
}

/// Transcript inserted by the composer's capture picker.
#[tauri::command]
pub async fn get_slack_thread_prompt_text(
    state: State<'_, SlackThreadsState>,
    id: String,
    include_original: bool,
) -> CmdResult<SlackPromptText> {
    let ctx = state.ctx();
    store::prompt_text(&ctx.db, &ctx.app_data, &id, include_original)
        .await
        .map_err(err)
}

/// Link a thread to a session (idempotent). A thread without a project takes
/// the run's.
#[tauri::command]
pub async fn link_slack_thread_run(
    state: State<'_, SlackThreadsState>,
    thread_id: String,
    run_id: String,
    source: String,
) -> CmdResult<LinkRunResult> {
    let r = run_links::link(&state.ctx().db, &thread_id, &run_id, &source)
        .await
        .map_err(err)?;
    if r.created || r.project_set {
        state.ctx().emit_changed(&thread_id, "updated");
    }
    Ok(r)
}

#[tauri::command]
pub async fn unlink_slack_thread_run(
    state: State<'_, SlackThreadsState>,
    thread_id: String,
    run_id: String,
) -> CmdResult<()> {
    if run_links::unlink(&state.ctx().db, &thread_id, &run_id)
        .await
        .map_err(err)?
    {
        state.ctx().emit_changed(&thread_id, "updated");
    }
    Ok(())
}

/// Sessions linked to a thread (runs that still exist).
#[tauri::command]
pub async fn list_slack_thread_runs(
    state: State<'_, SlackThreadsState>,
    thread_id: String,
) -> CmdResult<Vec<SlackThreadRunLink>> {
    run_links::runs_of_thread(&state.ctx().db, &thread_id)
        .await
        .map_err(err)
}

/// Threads linked to a session.
#[tauri::command]
pub async fn list_run_slack_threads(
    state: State<'_, SlackThreadsState>,
    run_id: String,
) -> CmdResult<Vec<SlackThreadSummary>> {
    run_links::threads_of_run(&state.ctx().db, &run_id)
        .await
        .map_err(err)
}
