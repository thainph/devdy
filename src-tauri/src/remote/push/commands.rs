//! Desktop (Settings → Remote Control) commands for Web Push: the subscribed
//! devices, the per-kind switches, and a test send.

use super::coalesce::PushEvents;
use super::send::Outcome;
use super::store::{self, DeviceInfo};
use crate::db::Db;
use crate::remote::RemoteState;
use serde::Serialize;
use tauri::State;

#[derive(Debug, Serialize)]
pub struct PushOverview {
    pub devices: Vec<DeviceInfo>,
    pub events: PushEvents,
}

#[tauri::command]
pub async fn remote_push_overview(db: State<'_, Db>) -> Result<PushOverview, String> {
    let devices = store::list(db.inner()).await?;
    Ok(PushOverview {
        devices: devices.iter().map(DeviceInfo::from).collect(),
        events: store::events(db.inner()).await,
    })
}

#[tauri::command]
pub async fn remote_push_remove(db: State<'_, Db>, id: String) -> Result<bool, String> {
    // A tombstone, not a delete: the phone's automatic re-subscribe must not
    // bring it back (turning notifications on again on the phone does).
    Ok(store::mark_removed(db.inner(), &id, &chrono::Utc::now().to_rfc3339()).await)
}

#[tauri::command]
pub async fn remote_push_set_events(db: State<'_, Db>, events: PushEvents) -> Result<(), String> {
    store::set_events(db.inner(), events).await
}

/// Send the test notification to one device. `Err` carries a short reason
/// (`not_found` when the push service no longer knows it — it is then removed).
#[tauri::command]
pub async fn remote_push_test(db: State<'_, Db>, state: State<'_, RemoteState>, id: String) -> Result<(), String> {
    let sub = store::by_id(db.inner(), &id).await.ok_or("not_found")?;
    match super::send_test(&state.push, db.inner(), &sub).await {
        Outcome::Delivered => Ok(()),
        Outcome::Gone => Err("not_found".into()),
        Outcome::Unauthorized => Err("unauthorized".into()),
        Outcome::Failed(reason) => Err(reason),
    }
}
