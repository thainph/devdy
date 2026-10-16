use crate::commands::translate::TranslateState;
use crate::db::Db;
use crate::inbox_api::{self, server, InboxApiInfo, InboxApiState};
use crate::slack_threads::SlackThreadsState;
use tauri::{AppHandle, State};

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

/// Turn the whole Inbox API on or off: persists `inbox_api_enabled`, then
/// starts the server (binding a port) or stops it (nothing listens).
#[tauri::command]
pub async fn set_inbox_api_enabled(
    app: AppHandle,
    db: State<'_, Db>,
    api: State<'_, InboxApiState>,
    translate: State<'_, TranslateState>,
    slack: State<'_, SlackThreadsState>,
    enabled: bool,
) -> Result<InboxApiInfo, String> {
    server::set_enabled(
        app,
        db.inner().clone(),
        api.inner().clone(),
        translate.inner().clone(),
        slack.inner().clone(),
        enabled,
    )
    .await?;
    Ok(api.info())
}
