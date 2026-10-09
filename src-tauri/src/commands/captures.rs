use crate::db::Db;
use crate::inbox_api::{
    self, emit_changed,
    ingest::{self, CaptureKind},
    server::app_data_dir,
    store::{self, Capture, CaptureDetail, IngestResult, Overrides},
    InboxApiInfo, InboxApiState,
};
use tauri::{AppHandle, State};

/// List captures (top-priority first), optionally filtered by kind
/// (`slack` | `web`) and project.
#[tauri::command]
pub async fn list_captures(
    db: State<'_, Db>,
    kind: Option<CaptureKind>,
    project_id: Option<String>,
) -> Result<Vec<Capture>, String> {
    store::list(db.inner(), kind, project_id.as_deref())
        .await
        .map_err(|e| e.to_string())
}

/// One capture with its attachments (attachment `filePath`s are absolute).
#[tauri::command]
pub async fn get_capture(
    app: AppHandle,
    db: State<'_, Db>,
    id: String,
) -> Result<CaptureDetail, String> {
    store::get_detail(db.inner(), &app_data_dir(&app)?, &id).await
}

/// Rename a capture. The title is marked user-chosen, so re-exports of the
/// same thread/page keep it (unless the extension sends `X-Devdy-Title`).
#[tauri::command]
pub async fn rename_capture(
    app: AppHandle,
    db: State<'_, Db>,
    id: String,
    title: String,
) -> Result<Capture, String> {
    let capture = store::rename(db.inner(), &id, &title).await?;
    emit_changed(&app, &id, capture.kind, "updated");
    Ok(capture)
}

#[tauri::command]
pub async fn update_capture(
    app: AppHandle,
    db: State<'_, Db>,
    id: String,
    title: Option<String>,
    content: Option<String>,
) -> Result<Capture, String> {
    let capture = store::update(db.inner(), &id, title, content).await?;
    emit_changed(&app, &id, capture.kind, "updated");
    Ok(capture)
}

/// Link or unlink (`None`) a capture from a project.
#[tauri::command]
pub async fn set_capture_project(
    app: AppHandle,
    db: State<'_, Db>,
    id: String,
    project_id: Option<String>,
) -> Result<(), String> {
    let kind = store::set_project(db.inner(), &id, project_id).await?;
    emit_changed(&app, &id, kind, "updated");
    Ok(())
}

/// Bulk-delete captures: rows, attachment rows and their files on disk.
#[tauri::command]
pub async fn delete_captures(
    app: AppHandle,
    db: State<'_, Db>,
    ids: Vec<String>,
) -> Result<(), String> {
    let deleted = store::delete(db.inner(), &app_data_dir(&app)?, &ids).await?;
    for (id, kind) in &deleted {
        emit_changed(&app, id, *kind, "deleted");
    }
    Ok(())
}

/// Persist a new top-to-bottom order (positions become list indexes).
#[tauri::command]
pub async fn reorder_captures(
    app: AppHandle,
    db: State<'_, Db>,
    ids: Vec<String>,
) -> Result<(), String> {
    store::reorder(db.inner(), &ids).await?;
    for (id, kind) in store::kinds_of(db.inner(), &ids).await? {
        emit_changed(&app, &id, kind, "updated");
    }
    Ok(())
}

/// Manual import of an exported `.zip` / `.md` as a capture of `kind` — same
/// parsing and upsert as the HTTP API.
#[tauri::command]
pub async fn import_capture_file(
    app: AppHandle,
    db: State<'_, Db>,
    path: String,
    kind: CaptureKind,
) -> Result<IngestResult, String> {
    let path = std::path::PathBuf::from(path);
    let format = ingest::PayloadKind::from_path(&path)
        .ok_or_else(|| "only .zip or .md files can be imported".to_string())?;
    let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
    if size > ingest::MAX_BODY_BYTES as u64 {
        return Err(format!(
            "file exceeds {} MB",
            ingest::MAX_BODY_BYTES / 1024 / 1024
        ));
    }
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    let parsed = ingest::parse_payload(kind, format, &bytes).map_err(|e| e.to_string())?;
    let result = store::ingest(db.inner(), &app_data_dir(&app)?, parsed, Overrides::default())
        .await
        .map_err(|e| e.to_string())?;
    emit_changed(&app, &result.id, result.kind, result.status.as_str());
    Ok(result)
}

#[tauri::command]
pub async fn get_inbox_api_info(api: State<'_, InboxApiState>) -> Result<InboxApiInfo, String> {
    Ok(api.info())
}

/// Issue a new API token. The old token stops working immediately (the server
/// reads it from managed state on every request).
#[tauri::command]
pub async fn regenerate_inbox_api_token(
    db: State<'_, Db>,
    api: State<'_, InboxApiState>,
) -> Result<InboxApiInfo, String> {
    let token = inbox_api::generate_token();
    inbox_api::save_token(db.inner(), &token)
        .await
        .map_err(|e| e.to_string())?;
    api.set_token(token);
    Ok(api.info())
}
