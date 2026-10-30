//! Two-way sync commands (design §3, §7), used by Settings → Sync. Sync is
//! on demand (on-demand-sync.md §2, §P): `sync_run` pulls the other device's
//! change feed and the files it needs straight from it (the relay only
//! forwards); nothing moves on its own.
//!
//! Group secrets live in the Keychain (`secrets::SyncSecrets`). The pause and
//! "sync secrets" flags are machine-local `settings` keys (`sync_paused`,
//! `sync_secrets_enabled`) and must never be synced themselves. Device names
//! and the leader device are synced settings (`sync::leader`).

use crate::db::Db;
use crate::sync::engine::{
    EngineHandle, PeerHandlers, RunPhase, RunProgress, StatusSink, SyncEngine, SyncRunReport, SyncState, SyncStatus,
    Timing, RUN_EVENT, STATUS_EVENT,
};
use crate::sync::feed::{FileScope, PullOptions};
use crate::sync::peer::{PeerHandler, PeerLink};
use crate::sync::leader::{self, DeviceInfo};
use crate::sync::store::{self, ParkedRow, SqliteSyncStore, SyncStore};
use crate::sync::transport::{Connection, TransportError};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Mutex;

const KEY_PAUSED: &str = "sync_paused";
/// How long `sync_set_leader` waits for the new leader to confirm.
const LEADER_CONFIRM_WAIT: Duration = Duration::from_secs(30);
/// Retry cadence of an unconfirmed leader hand-off.
const LEADER_RETRY_EVERY: Duration = Duration::from_secs(60);
/// How often a leader change arrived through sync is looked for.
const LEADER_WATCH_EVERY: Duration = Duration::from_secs(30);
const KEY_SECRETS: &str = crate::sync::secrets::SETTING;

/// App-wide sync state (Tauri managed): the running engine, if any, and the
/// handlers of other devices' requests.
#[derive(Clone, Default)]
pub struct SyncManager {
    engine: Arc<Mutex<Option<SyncEngine>>>,
    handlers: PeerHandlers,
    /// Drives an unconfirmed leader hand-off while the engine runs.
    handoff: Arc<std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    handoff_kick: Arc<tokio::sync::Notify>,
}

impl SyncManager {
    /// Serve another device's requests of `handler.kinds()` (on-demand sync
    /// features). Callable before or after the engine starts; the first
    /// handler registered for a kind wins.
    pub fn register_peer_handler(&self, handler: Arc<dyn PeerHandler>) {
        if let Ok(mut h) = self.handlers.write() {
            h.push(handler);
        }
    }

    /// The running engine (cheap handle: long operations do not hold the slot).
    pub async fn handle(&self) -> Option<EngineHandle> {
        self.engine.lock().await.as_ref().map(SyncEngine::handle)
    }

    /// Requests to the other device, while the engine runs.
    pub async fn peer_link(&self) -> Option<Arc<dyn PeerLink>> {
        self.handle().await.map(|h| h.link())
    }

    /// Start the engine when this device is in a group (no-op otherwise or
    /// when already running).
    pub async fn start(&self, app: &AppHandle, db: &Db) -> Result<(), String> {
        let mut slot = self.engine.lock().await;
        if slot.is_some() {
            return Ok(());
        }
        let Some(sync) = crate::secrets::get_sync_secrets() else {
            return Ok(());
        };
        // Joined but not merged yet: serving now would hand out local ids.
        if crate::sync::bootstrap::is_pending(db).await? {
            return Ok(());
        }
        let params = crate::sync::pair::connect_params(db, &sync).await?;
        let keys = crate::sync::pair::keys_of(&sync)?;
        let paths = crate::sync::codec::PathCtx::from_env()
            .with_app_data(app.path().app_data_dir().map_err(|e| format!("no app data dir: {e}"))?);
        let store: Arc<dyn SyncStore> = Arc::new(
            SqliteSyncStore::new(db.clone(), sync.device_id.clone())
                .with_paths(paths)
                .with_secrets(Arc::new(crate::sync::secrets::KeychainBackend))
                .with_notifier(crate::sync::events::app_notifier(app.clone())),
        );
        let paused = crate::remote::get_setting(db, KEY_PAUSED).await.as_deref() == Some("true");
        let emitter = app.clone();
        let sink: StatusSink = Arc::new(move |s: &SyncStatus| {
            let _ = emitter.emit(STATUS_EVENT, s);
        });
        *slot = Some(SyncEngine::start_with(
            params,
            keys,
            store,
            sink,
            paused,
            Timing::default(),
            self.handlers.clone(),
        ));
        leader::set_sync_active(true);
        self.start_handoff_loop(db, sync.device_id.clone());
        // The group shows this device by its Remote Control alias, the name
        // another device types to join through it.
        let me = &sync.device_id;
        let res = match crate::remote::device_alias(db).await {
            Some(alias) if leader::record(db, me).await.map(|r| r.name).as_deref() != Some(alias.as_str()) => {
                leader::rename_self(db, me, &alias).await
            }
            _ => leader::touch_self(db, me, &default_device_name(db).await).await,
        };
        if let Err(e) = res {
            tracing::warn!(event = "sync_device_record_failed", error = %e);
        }
        Ok(())
    }

    /// The Remote Control alias changed: rename this device in the group.
    /// Joined but not merged: `start` picks the alias up after the merge.
    /// A cleared alias keeps the last name.
    pub async fn alias_changed(&self, db: &Db, alias: &str) -> Result<(), String> {
        let Some(sync) = crate::secrets::get_sync_secrets().filter(|_| !alias.is_empty()) else {
            return Ok(());
        };
        if crate::sync::bootstrap::is_pending(db).await? {
            return Ok(());
        }
        leader::rename_self(db, &sync.device_id, alias).await?;
        self.kick().await;
        Ok(())
    }

    pub async fn stop(&self) {
        if let Some(engine) = self.engine.lock().await.take() {
            engine.stop().await;
        }
        if let Some(t) = self.handoff.lock().ok().and_then(|mut t| t.take()) {
            t.abort();
        }
        leader::set_sync_active(false);
    }

    /// While the engine runs: notice the leader role leaving this device (set
    /// here or arrived through sync) and tell the new leader until it confirms
    /// (`leader::handoff_step`); this device keeps the singleton jobs
    /// meanwhile. The hand-off is persisted, so a restart resumes it.
    fn start_handoff_loop(&self, db: &Db, me: String) {
        let (manager, db) = (self.clone(), db.clone());
        let task = tokio::spawn(async move {
            loop {
                let notify = |to: String| {
                    let manager = manager.clone();
                    async move {
                        match manager.handle().await {
                            Some(h) => h.notify_leader(&to).await,
                            None => Err("not in a sync group".to_string()),
                        }
                    }
                };
                let pending = leader::handoff_step(&db, &me, notify).await.unwrap_or_else(|e| {
                    tracing::warn!(event = "sync_leader_handoff_failed", error = %e);
                    true
                });
                let wait = if pending { LEADER_RETRY_EVERY } else { LEADER_WATCH_EVERY };
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = manager.handoff_kick.notified() => {}
                }
            }
        });
        if let Ok(mut slot) = self.handoff.lock() {
            if let Some(old) = slot.replace(task) {
                old.abort();
            }
        }
    }

    /// Something worth syncing changed. Sync is on demand, so this only
    /// retries the relay connection right away when it is down. Cheap; safe
    /// to call from anywhere.
    pub async fn kick(&self) {
        if let Some(e) = self.engine.lock().await.as_ref() {
            e.kick();
        }
    }

    /// The other device of the group, when it is connected now.
    async fn online_peer(&self) -> Option<String> {
        let h = self.handle().await?;
        h.online_peers().await.ok()?.into_iter().next()
    }

    /// Tell `to` it leads now (or simply: to pull from this device) and wait
    /// (at most `wait`) until it synced that.
    async fn notify_leader(&self, to: &str, wait: Duration) -> bool {
        let Some(h) = self.handle().await else { return false };
        matches!(tokio::time::timeout(wait, h.notify_leader(to)).await, Ok(Ok(())))
    }

    /// Wait (at most `wait`) until the engine is connected.
    async fn wait_online(&self, wait: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let s = self.status().await;
            if s.state == SyncState::Online && !s.paused {
                return true;
            }
            if s.state == SyncState::Disabled || tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Pause / resume the running engine without touching the stored setting.
    async fn set_engine_paused(&self, paused: bool) {
        if let Some(e) = self.engine.lock().await.as_ref() {
            e.set_paused(paused);
        }
    }

    async fn status(&self) -> SyncStatus {
        match self.engine.lock().await.as_ref() {
            Some(e) => e.status(),
            None => SyncStatus::disabled(),
        }
    }
}

fn emit_disabled(app: &AppHandle) {
    let _ = app.emit(STATUS_EVENT, SyncStatus::disabled());
}

#[tauri::command]
pub async fn sync_status(state: State<'_, SyncManager>) -> Result<SyncStatus, String> {
    Ok(state.status().await)
}

/// First device: create a new group on the relay and start syncing.
#[tauri::command]
pub async fn sync_create_group(
    app: AppHandle,
    db: State<'_, Db>,
    state: State<'_, SyncManager>,
) -> Result<SyncStatus, String> {
    if crate::secrets::get_sync_secrets().is_some() {
        return Err("this device is already in a sync group".into());
    }
    let sync = crate::sync::pair::create_group(db.inner()).await?;
    crate::sync::bootstrap::prepare_new_group(db.inner()).await?;
    // The creator leads singleton jobs until someone picks another device.
    leader::claim_group(db.inner(), &sync.device_id).await?;
    state.start(&app, db.inner()).await?;
    Ok(state.status().await)
}

/// Whether this machine has its own relay URL + token (Settings → Remote
/// Control), which creating or joining a group needs.
#[tauri::command]
pub async fn sync_relay_ready(db: State<'_, Db>) -> Result<bool, String> {
    Ok(crate::sync::pair::relay_ready(db.inner()).await)
}

/// Join the group of the device named `device_alias` with its Remote Control
/// master password. That device's user must have opened "Accept a new
/// device" and approve the request, which shows this device's name.
#[tauri::command]
pub async fn sync_join(
    db: State<'_, Db>,
    state: State<'_, SyncManager>,
    device_alias: String,
    password: String,
) -> Result<SyncStatus, String> {
    if crate::secrets::get_sync_secrets().is_some() {
        return Err("this device is already in a sync group".into());
    }
    let alias = device_alias.trim();
    if !crate::remote::login::valid_alias(alias) {
        return Err("invalid device alias".into());
    }
    if password.is_empty() {
        return Err("password is required".into());
    }
    if !crate::sync::pair::relay_ready(db.inner()).await {
        return Err("relay_not_configured: set the relay URL and token in Settings → Remote Control first".into());
    }
    let name = default_device_name(db.inner()).await;
    // Leaves the merge pending: the engine starts after sync_bootstrap_commit.
    crate::sync::pair::join(db.inner(), alias, password, &name).await?;
    Ok(state.status().await)
}

/// `sync_leave` refused: this device holds the leader / creator role and the
/// other device could not take the hand-off (offline). Leaving anyway
/// (`force`) leaves it waiting on a departed leader.
pub const LEAVE_HANDOFF_PENDING: &str = "sync_leave_handoff_pending";

/// How long leaving waits for the relay connection.
const LEAVE_CONNECT_WAIT: Duration = Duration::from_secs(10);
/// How long leaving waits for the other device to pull the hand-off.
const LEAVE_LEADER_WAIT: Duration = Duration::from_secs(60);

/// Review-3 M5: whether leaving may go on. The relay keeps nothing, so the
/// hand-off only reaches the other device when it pulls it from this one
/// before it leaves: a role holder whose hand-off was not pulled must not
/// leave silently.
fn leave_gate(holds_role: bool, handed_off: bool, force: bool) -> Result<(), String> {
    if holds_role && !handed_off && !force {
        return Err(format!(
            "{LEAVE_HANDOFF_PENDING}: this device leads the group's singleton jobs or created the group, \
             and the relay could not be reached to hand that on"
        ));
    }
    Ok(())
}

/// Leave the group: revoke this device on the relay and wipe the Keychain
/// entry. Returns whether the relay confirmed the revoke (false = offline;
/// the local membership is gone either way). A device holding the leader /
/// creator role first has the other device pull the hand-off from it
/// (resuming a paused engine for that) and refuses with
/// [`LEAVE_HANDOFF_PENDING`] when that fails, unless `force`.
#[tauri::command]
pub async fn sync_leave(
    app: AppHandle,
    db: State<'_, Db>,
    state: State<'_, SyncManager>,
    force: Option<bool>,
) -> Result<bool, String> {
    let force = force.unwrap_or(false);
    // Hand the leader / creator roles on and drop this device's record while
    // the other device can still pull it; the relay is told the creator's
    // successor.
    let mut successor = None;
    if let Some(sync) = crate::secrets::get_sync_secrets() {
        let me = sync.device_id.as_str();
        let is_creator = leader::creator(db.inner()).await.as_deref() == Some(me);
        let holds_role = is_creator || leader::leader(db.inner()).await.as_deref() == Some(me);
        let paused = crate::remote::get_setting(db.inner(), KEY_PAUSED).await.as_deref() == Some("true");
        if paused {
            state.set_engine_paused(false).await;
        }
        // Without a role, only this device's own record is at stake.
        let online = state.wait_online(if holds_role { LEAVE_CONNECT_WAIT } else { LEAVE_CONNECT_WAIT / 2 }).await;
        let mut handed_off = false;
        if online {
            match leader::on_leave(db.inner(), me).await {
                Ok(next) => {
                    successor = next;
                    // The other device (the new leader, if any) takes the
                    // role and this device's departure by pulling them now,
                    // before this device (which stops the jobs here) is gone.
                    let new_leader = leader::leader(db.inner()).await.filter(|l| l != me);
                    let to = match new_leader {
                        Some(l) => Some(l),
                        None => state.online_peer().await,
                    };
                    if let Some(to) = to {
                        handed_off = state.notify_leader(&to, LEAVE_LEADER_WAIT).await;
                    }
                }
                Err(e) => tracing::warn!(event = "sync_leave_handover_failed", error = %e),
            }
        }
        if let Err(e) = leave_gate(holds_role, handed_off, force) {
            // Still a member: the other device pulls the hand-off later.
            if paused {
                state.set_engine_paused(true).await;
            }
            return Err(e);
        }
        if successor.is_none() && is_creator {
            successor = leader::leave_successor(db.inner(), me).await;
        }
    }
    crate::sync::pair::close_accept_window();
    state.stop().await;
    leader::forget_local(db.inner()).await?;
    let revoked = crate::sync::pair::leave(db.inner(), successor.as_deref()).await?;
    emit_disabled(&app);
    Ok(revoked)
}

#[tauri::command]
pub async fn sync_pause(
    db: State<'_, Db>,
    state: State<'_, SyncManager>,
    paused: bool,
) -> Result<SyncStatus, String> {
    crate::remote::set_setting(db.inner(), KEY_PAUSED, if paused { "true" } else { "false" }).await?;
    if let Some(e) = state.engine.lock().await.as_ref() {
        e.set_paused(paused);
    }
    Ok(state.status().await)
}

/// Reconnect to the relay now when the connection is down. Sync itself is
/// on demand: see `sync_run`.
#[tauri::command]
pub async fn sync_now(state: State<'_, SyncManager>) -> Result<(), String> {
    match state.engine.lock().await.as_ref() {
        Some(e) => {
            e.kick();
            Ok(())
        }
        None => Err("this device is not in a sync group".into()),
    }
}

/// Payload of `sync://run`: the phase plus pages / ops received and file
/// bytes / items read so far.
#[derive(Debug, Clone, serde::Serialize)]
struct RunEvent {
    phase: RunPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(flatten)]
    progress: RunProgress,
}

/// Manual sync from `source_device`: this device pulls its change feed page
/// by page and the files it takes chunk by chunk, applying as it goes (one
/// direction). Progress goes out as `sync://run`. Errors are returned as is;
/// `peer_offline` (the source is not running Devdy / not connected),
/// `peer_paused`, `peer_timeout` and `sync_not_connected` are bare codes.
#[tauri::command]
pub async fn sync_run(
    app: AppHandle,
    db: State<'_, Db>,
    state: State<'_, SyncManager>,
    source_device: String,
) -> Result<SyncRunReport, String> {
    let emit = |phase: RunPhase, message: Option<String>, progress: &RunProgress| {
        let _ = app.emit(RUN_EVENT, RunEvent { phase, message, progress: progress.clone() });
    };
    // Session files are the bulk of the data: the source rescans them only
    // when this device takes sessions.
    let sessions_off = {
        let mut conn = db.acquire().await.map_err(|e| e.to_string())?;
        crate::sync::prefs::categories_off(&mut conn).await?.iter().any(|c| c == "sessions")
    };
    let scope = if sessions_off { FileScope::NoSessions } else { FileScope::All };
    let opts = PullOptions { files: scope, fetch: scope };
    let result = match state.handle().await {
        Some(h) => h.run_from(&source_device, opts, &|p, progress| emit(p, None, progress)).await,
        None => Err("this device is not in a sync group".into()),
    };
    // A leader change may have arrived.
    state.handoff_kick.notify_one();
    match &result {
        Ok(r) => {
            let done = RunProgress { pages: r.pages, ops: r.pulled as u64, done_bytes: r.bytes, total_bytes: r.bytes, ..Default::default() };
            emit(RunPhase::Done, None, &done)
        }
        Err(e) => emit(RunPhase::Error, Some(e.clone()), &RunProgress::default()),
    }
    result
}

/// Devices of the group connected to the relay right now (not this one).
#[tauri::command]
pub async fn sync_peers_online(state: State<'_, SyncManager>) -> Result<Vec<String>, String> {
    match state.handle().await {
        Some(h) => h.online_peers().await,
        None => Err("this device is not in a sync group".into()),
    }
}

/// Whether Keychain secrets are synced (D4: default on when never set).
#[tauri::command]
pub async fn sync_get_secrets(db: State<'_, Db>) -> Result<bool, String> {
    Ok(crate::remote::get_setting(db.inner(), KEY_SECRETS).await.as_deref() != Some("false"))
}

/// Whether Keychain secrets (and managed SSH key files) are synced (D4,
/// default on).
#[tauri::command]
pub async fn sync_set_secrets(db: State<'_, Db>, enabled: bool) -> Result<(), String> {
    crate::remote::set_setting(db.inner(), KEY_SECRETS, if enabled { "true" } else { "false" }).await
}

/// Name a device gets until the user renames it: the Remote Control alias,
/// else the computer name.
async fn default_device_name(db: &Db) -> String {
    if let Some(alias) = crate::remote::device_alias(db).await {
        return alias;
    }
    let computer = tokio::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .await
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|n| !n.is_empty());
    computer.unwrap_or_else(|| "Devdy".to_string())
}

fn group_device_id() -> Result<String, String> {
    crate::secrets::get_sync_secrets()
        .map(|s| s.device_id)
        .ok_or_else(|| "this device is not in a sync group".to_string())
}

/// Devices of the group (from their synced name records), this one first.
#[tauri::command]
pub async fn sync_devices_list(db: State<'_, Db>) -> Result<Vec<DeviceInfo>, String> {
    let me = group_device_id()?;
    leader::list_devices(db.inner(), &me).await
}

const ONLY_CREATOR: &str = "only the device that created the group can remove other devices";

/// Remove another device from the group: revoke its token on the relay, then
/// drop its record (the leader role passes to this device if it held it).
/// Only the group creator may do this (the relay enforces it). The removed
/// device keeps the group key but can no longer reach the relay's data.
#[tauri::command]
pub async fn sync_revoke_device(
    db: State<'_, Db>,
    state: State<'_, SyncManager>,
    device_id: String,
) -> Result<(), String> {
    let sync = crate::secrets::get_sync_secrets().ok_or("this device is not in a sync group")?;
    if device_id == sync.device_id {
        return Err("use Leave group to remove this device".into());
    }
    if leader::creator(db.inner()).await.is_some_and(|c| c != sync.device_id) {
        return Err(ONLY_CREATOR.into());
    }
    let params = crate::sync::pair::connect_params(db.inner(), &sync).await?;
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let conn = Connection::connect(&params, false, tx).await.map_err(|e| match e {
        TransportError::Rejected(_) => e.to_string(),
        other => format!("relay unreachable: {other}"),
    })?;
    conn.revoke(&device_id).await.map_err(|e| match e {
        TransportError::Rejected(c) | TransportError::Relay(c) if c == "forbidden" => ONLY_CREATOR.to_string(),
        other => other.to_string(),
    })?;
    drop(conn);
    leader::forget_device(db.inner(), &device_id, &sync.device_id).await?;
    tracing::info!(event = "sync_device_revoked");
    state.kick().await;
    Ok(())
}

/// Pick the device that runs singleton jobs (Slack polling); `None`
/// lets every device run them. The new leader is always told to sync from
/// this device at once; returns whether it confirmed. The device that was
/// the leader — this one, or another that learns of the change when it
/// syncs — keeps running the jobs until the new leader confirms
/// (`leader::observe_change`).
#[tauri::command]
pub async fn sync_set_leader(
    db: State<'_, Db>,
    state: State<'_, SyncManager>,
    device_id: Option<String>,
) -> Result<bool, String> {
    let db = db.inner();
    let me = group_device_id().ok();
    if let Some(me) = &me {
        // Record who leads before the change, so losing the role is noticed.
        leader::observe_change(db, me).await?;
    }
    leader::set_leader(db, device_id.as_deref()).await?;
    if let Some(me) = &me {
        leader::observe_change(db, me).await?;
    }
    let Some(to) = device_id.filter(|d| Some(d) != me.as_ref()) else {
        return Ok(true);
    };
    let confirmed = state.notify_leader(&to, LEADER_CONFIRM_WAIT).await;
    if confirmed {
        leader::handoff_confirmed(db, &to).await?;
    } else {
        state.handoff_kick.notify_one();
    }
    Ok(confirmed)
}

/// Open "Accept a new device" for `sync::pair::ACCEPT_WINDOW`; returns the
/// expiry (unix millis). Join requests arriving meanwhile still need the
/// user's approval (`sync://join-request` → `sync_join_respond`).
#[tauri::command]
pub async fn sync_accept_open(app: AppHandle) -> Result<i64, String> {
    if crate::secrets::get_sync_secrets().is_none() {
        return Err("this device is not in a sync group".into());
    }
    let expires = crate::sync::pair::open_accept_window();
    let _ = app.emit(crate::sync::pair::ACCEPT_WINDOW_EVENT, json!({ "expires_at": expires }));
    Ok(expires)
}

/// Close the window early (a request still waiting is denied).
#[tauri::command]
pub async fn sync_accept_close(app: AppHandle) -> Result<(), String> {
    crate::sync::pair::close_accept_window();
    let _ = app.emit(crate::sync::pair::ACCEPT_WINDOW_EVENT, json!({ "expires_at": null }));
    Ok(())
}

/// The open window's expiry (unix millis), if any.
#[tauri::command]
pub async fn sync_accept_state() -> Result<Option<i64>, String> {
    Ok(crate::sync::pair::accept_window_expires())
}

/// Answer a pending join request.
#[tauri::command]
pub async fn sync_join_respond(request_id: String, allow: bool) -> Result<(), String> {
    crate::sync::pair::respond_join(&request_id, allow)
}

/// How many parked rows `sync_parked_list` returns at most.
const PARKED_LIST_LIMIT: usize = 200;

/// Rows too large to be served to the other device (newest first).
#[tauri::command]
pub async fn sync_parked_list(db: State<'_, Db>) -> Result<Vec<ParkedRow>, String> {
    store::parked_list(db.inner(), PARKED_LIST_LIMIT).await
}

/// Queue the parked rows again (offered with the next Sync); returns how
/// many. Rows still too large are parked again.
#[tauri::command]
pub async fn sync_parked_retry(db: State<'_, Db>, state: State<'_, SyncManager>) -> Result<usize, String> {
    let n = store::parked_retry(db.inner()).await?;
    if let Some(e) = state.engine.lock().await.as_ref() {
        e.set_parked(0);
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_role_holder_leaves_only_once_the_hand_off_is_out() {
        // Review-3 M5: offline / paused leader or creator is refused…
        let e = leave_gate(true, false, false).unwrap_err();
        assert!(e.starts_with(LEAVE_HANDOFF_PENDING), "{e}");
        // …unless the user insists, or the hand-off went out.
        assert!(leave_gate(true, false, true).is_ok());
        assert!(leave_gate(true, true, false).is_ok());
        // Nothing to hand on: leave offline as before.
        assert!(leave_gate(false, false, false).is_ok());
    }
}
