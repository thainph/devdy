//! Sync group membership: create, join (over the password-login channel),
//! leave (design §3 "Pairing").
//!
//! Group state lives in the Keychain (`secrets::SyncSecrets`): group id, the
//! 32-byte group key, this device's relay token and its sync device id.
//!
//! Every membership gets a fresh sync device id ([`new_membership_device`]):
//! the relay remembers a revoked id forever, so leaving and joining again
//! needs a new one. HLCs order by (millis, counter) first and use the device
//! only as a tie-breaker, so rows stamped with the old id stay valid.
//!
//! Join reuses Remote Control's password login: the new device dials the
//! relay as a Controller on the target host's login code, runs the same
//! X25519 handshake keyed by `PBKDF2(master password)` (a Rust port of
//! `src/controller/login.ts`), and sends a sealed `{kind:'cmd',
//! action:'sync_join', device_name}` instead of the plain login proof. The
//! password alone is not enough: the host grants only while the user has
//! opened an "accept a new device" window in Settings → Sync AND approves the
//! named request ([`host_join`], called from `remote::agent`'s login path).
//! The grant is `{group_id, group_key, ticket}`; the host first announces the
//! ticket's hash to the relay (`sync_invite`), and the joiner's first hello
//! carries the ticket — the relay registers unknown devices only that way.
//! The relay host token is never handed over: the joiner needs its own relay
//! URL + token (Remote Control settings).

use super::crypto::{generate_group_key, SyncKeys, KEY_LEN};
use super::transport::{ConnectParams, Connection, TransportError, CONNECT_TIMEOUT};
use crate::db::Db;
use crate::remote::protocol::{Envelope, FrameType, HandshakePayload};
use crate::secrets::SyncSecrets;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use remote_e2e::{host_fingerprint, Keypair, Session};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

/// The sealed command a joining device sends on the login channel.
pub const SYNC_JOIN_ACTION: &str = "sync_join";
/// Same budget as the web controller's password login.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(20);
/// How long Settings → Sync "Accept a new device" stays open.
pub const ACCEPT_WINDOW: Duration = Duration::from_secs(5 * 60);
/// How long the host waits for the user to answer one join request.
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);
/// The joiner's wait once the host said the request is pending.
const PENDING_TIMEOUT: Duration = Duration::from_secs(150);
/// Lifetime of a join ticket on the relay (the joiner uses it right away).
const TICKET_TTL_SECS: u64 = 300;
const MAX_DEVICE_NAME: usize = 64;

pub const JOIN_REQUEST_EVENT: &str = "sync://join-request";
pub const JOIN_RESOLVED_EVENT: &str = "sync://join-resolved";
pub const ACCEPT_WINDOW_EVENT: &str = "sync://accept-window";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinError {
    /// Nobody listening on that login code (offline, no master password, wrong alias).
    HostOffline,
    WrongPassword,
    Network(String),
    /// The host is reachable but not in a sync group.
    HostNotInGroup,
    /// The host predates ticketed joins (or sync altogether).
    HostUnsupported,
    /// The host is not accepting new devices (Settings → Sync window closed).
    NotAccepting,
    /// The host's user declined (or did not answer in time).
    Declined,
    /// The host is already answering another join request.
    Busy,
    Other(String),
}

impl std::fmt::Display for JoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JoinError::HostOffline => f.write_str("host_offline: no device answered on that alias"),
            JoinError::WrongPassword => f.write_str("wrong_password"),
            JoinError::Network(e) => write!(f, "network: {e}"),
            JoinError::HostNotInGroup => f.write_str("host_not_in_group: that device has no sync group"),
            JoinError::HostUnsupported => {
                f.write_str("host_unsupported: that device's Devdy is too old to add devices; update it")
            }
            JoinError::NotAccepting => f.write_str(
                "not_accepting: open Settings → Sync → Accept a new device on that machine first",
            ),
            JoinError::Declined => f.write_str("declined: the request was declined on that machine"),
            JoinError::Busy => f.write_str("busy: that machine is answering another join request"),
            JoinError::Other(e) => f.write_str(e),
        }
    }
}

/// What a host hands a joining device. Never the relay host token.
#[derive(Clone)]
pub struct Grant {
    pub group_id: String,
    pub group_key: Vec<u8>,
    /// One-time relay registration ticket (its hash was announced by the host).
    pub ticket: String,
}

fn random_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    dryoc::rng::copy_randombytes(&mut b);
    hex::encode(b)
}

fn sha256_hex(s: &str) -> String {
    use sha2::Digest as _;
    hex::encode(sha2::Sha256::digest(s.as_bytes()))
}

/// `sync_state` key prefix of the device ids this machine had in earlier
/// memberships (value: unused). Runs still stamped with one are this
/// machine's: [`install_past_device_triggers`] maps them to the current id.
pub const PAST_DEVICE_PREFIX: &str = "past_device:";

/// Switch this machine to the sync device id `new` of a new membership (call
/// it once the relay accepted `new`): the `sync_device` row (HLCs use it), the
/// runs it owns, and drop the device record of the old id (a left device's
/// record would linger in the group). All of it under the apply guard: a
/// device id change is not an edit to push (pushed run rows would overwrite
/// another device's take-over). The old id is kept as a past id, so run rows
/// a peer sends back with it stay this machine's.
/// Rows already stamped with the old id keep their HLC: the device part is
/// only a tie-breaker. The persisted HLC watermark is device-independent.
pub async fn new_membership_device(db: &Db, new: &str) -> Result<(), String> {
    let old = super::ensure_device(db).await?;
    if old == new {
        return Ok(());
    }
    let e = |e: sqlx::Error| e.to_string();
    let mut tx = db.begin().await.map_err(e)?;
    sqlx::query("UPDATE sync_apply_guard SET active = 1 WHERE id = 1").execute(&mut *tx).await.map_err(e)?;
    sqlx::query("UPDATE sync_device SET id = ? WHERE id = ?")
        .bind(new)
        .bind(&old)
        .execute(&mut *tx)
        .await
        .map_err(e)?;
    sqlx::query("INSERT OR REPLACE INTO sync_state (key, value) VALUES (?, '1')")
        .bind(format!("{PAST_DEVICE_PREFIX}{old}"))
        .execute(&mut *tx)
        .await
        .map_err(e)?;
    sqlx::query(
        "UPDATE runs SET owner_device = ? WHERE owner_device IN \
         (SELECT substr(key, ?) FROM sync_state WHERE substr(key, 1, ?) = ?)",
    )
    .bind(new)
    .bind(PAST_DEVICE_PREFIX.len() as i64 + 1)
    .bind(PAST_DEVICE_PREFIX.len() as i64)
    .bind(PAST_DEVICE_PREFIX)
    .execute(&mut *tx)
    .await
    .map_err(e)?;
    sqlx::query("DELETE FROM settings WHERE key = ?")
        .bind(format!("{}{old}", super::leader::DEVICE_NAME_PREFIX))
        .execute(&mut *tx)
        .await
        .map_err(e)?;
    sqlx::query("UPDATE sync_apply_guard SET active = 0 WHERE id = 1").execute(&mut *tx).await.map_err(e)?;
    tx.commit().await.map_err(e)?;
    Ok(())
}

/// Runs that arrive (or change) owned by one of this machine's past device
/// ids get the current id. Only remote ops carry a past id, and those apply
/// under the guard, so the rewrite is never pushed.
pub async fn install_past_device_triggers(db: &Db) -> Result<(), String> {
    for (name, event) in [("sync_past_owner_ins", "INSERT"), ("sync_past_owner_upd", "UPDATE OF owner_device")] {
        let sql = format!(
            "CREATE TRIGGER IF NOT EXISTS {name} AFTER {event} ON runs \
             WHEN NEW.owner_device IS NOT NULL AND NEW.owner_device != '' \
              AND EXISTS (SELECT 1 FROM sync_state WHERE key = '{PAST_DEVICE_PREFIX}' || NEW.owner_device) \
             BEGIN UPDATE runs SET owner_device = (SELECT id FROM sync_device LIMIT 1) WHERE id = NEW.id; END"
        );
        sqlx::query(&sql).execute(db).await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Group-scoped synced settings (device names, leader, creator) of a group
/// this machine left: removed locally without pushing deletes, so they never
/// travel into the next group as ghosts.
async fn forget_group_settings(db: &Db) -> Result<(), String> {
    use super::leader::{DEVICE_NAME_PREFIX, KEY_CREATOR, KEY_LEADER};
    let e = |e: sqlx::Error| e.to_string();
    let mut tx = db.begin().await.map_err(e)?;
    sqlx::query("UPDATE sync_apply_guard SET active = 1 WHERE id = 1").execute(&mut *tx).await.map_err(e)?;
    sqlx::query("DELETE FROM settings WHERE substr(key, 1, ?) = ? OR key IN (?, ?)")
        .bind(DEVICE_NAME_PREFIX.len() as i64)
        .bind(DEVICE_NAME_PREFIX)
        .bind(KEY_LEADER)
        .bind(KEY_CREATOR)
        .execute(&mut *tx)
        .await
        .map_err(e)?;
    sqlx::query("DELETE FROM sync_outbox WHERE tbl = 'settings' AND (substr(pk, 2, ?) = ? OR pk IN (?, ?))")
        .bind(DEVICE_NAME_PREFIX.len() as i64)
        .bind(DEVICE_NAME_PREFIX)
        .bind(serde_json::Value::from(KEY_LEADER).to_string())
        .bind(serde_json::Value::from(KEY_CREATOR).to_string())
        .execute(&mut *tx)
        .await
        .map_err(e)?;
    sqlx::query("UPDATE sync_apply_guard SET active = 0 WHERE id = 1").execute(&mut *tx).await.map_err(e)?;
    tx.commit().await.map_err(e)?;
    Ok(())
}

/// Relay URL + host token from Remote Control's config. `sync` adds the
/// token an older build handed over at join, for memberships made then.
async fn relay_endpoint(db: &Db, sync: Option<&SyncSecrets>) -> Result<(String, String), String> {
    let url = crate::remote::get_setting(db, crate::remote::KEY_RELAY_URL)
        .await
        .filter(|u| !u.trim().is_empty())
        .ok_or("relay_not_configured: relay URL is not set (Settings → Remote Control)")?;
    let token = crate::secrets::get_remote_auth_token()
        .or_else(|| sync.and_then(|s| s.relay_token.clone()))
        .ok_or("relay_not_configured: relay token is not set (Settings → Remote Control)")?;
    Ok((url, token))
}

/// Whether this machine can create / join a group (own relay URL + token).
pub async fn relay_ready(db: &Db) -> bool {
    relay_endpoint(db, None).await.is_ok()
}

/// Connection parameters for a configured group.
pub async fn connect_params(db: &Db, sync: &SyncSecrets) -> Result<ConnectParams, String> {
    let (relay_url, auth_token) = relay_endpoint(db, Some(sync)).await?;
    Ok(ConnectParams {
        relay_url,
        auth_token,
        group_id: sync.group_id.clone(),
        device_id: sync.device_id.clone(),
        device_token: sync.device_token.clone(),
    })
}

pub fn keys_of(sync: &SyncSecrets) -> Result<SyncKeys, String> {
    let raw = hex::decode(&sync.group_key).map_err(|_| "stored group key is corrupt".to_string())?;
    SyncKeys::derive(&raw).map_err(|e| e.to_string())
}

/// Register on the relay with one hello and close.
async fn hello(params: &ConnectParams, create: bool, ticket: Option<&str>) -> Result<(), String> {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    Connection::connect_with_ticket(params, create, ticket, tx)
        .await
        .map(drop)
        .map_err(|e| e.to_string())
}

/// Become a member under the new device id `sync.device_id`: flag the
/// membership and the pending merge first (a crash after the Keychain write
/// then resumes at the merge step instead of syncing unmerged), register on
/// the relay, and only once it accepted switch this machine's device id and
/// store the group.
async fn enter_group(db: &Db, sync: &SyncSecrets, params: &ConnectParams, create: bool, ticket: Option<&str>) -> Result<(), String> {
    super::set_member(db, true).await?;
    super::bootstrap::mark_joined(db).await?;
    if let Err(e) = hello(params, create, ticket).await {
        let _ = super::bootstrap::clear_pending(db).await;
        let _ = super::set_member(db, false).await;
        return Err(e);
    }
    new_membership_device(db, &sync.device_id).await?;
    crate::secrets::set_sync_secrets(sync).map_err(|e| e.to_string())
}

/// First device: make a new group on the relay and store it. The caller runs
/// `bootstrap::prepare_new_group` next (until then the merge stays pending).
pub async fn create_group(db: &Db) -> Result<SyncSecrets, String> {
    let (relay_url, auth_token) = relay_endpoint(db, None).await?;
    let sync = SyncSecrets {
        group_id: uuid::Uuid::new_v4().to_string(),
        group_key: hex::encode(generate_group_key()),
        device_token: random_hex(32),
        device_id: uuid::Uuid::new_v4().simple().to_string(),
        relay_token: None,
    };
    let params = ConnectParams {
        relay_url,
        auth_token,
        group_id: sync.group_id.clone(),
        device_id: sync.device_id.clone(),
        device_token: sync.device_token.clone(),
    };
    enter_group(db, &sync, &params, true, None).await?;
    tracing::info!(event = "sync_group_created");
    Ok(sync)
}

/// Whether the relay takes this machine's host token (and has sync on),
/// checked before bothering the host's user: a hello for a group that does
/// not exist is refused with `group_not_found` only after the token passed.
pub async fn check_relay_auth(relay_url: &str, auth_token: &str) -> Result<(), String> {
    let params = ConnectParams {
        relay_url: relay_url.to_string(),
        auth_token: auth_token.to_string(),
        group_id: uuid::Uuid::new_v4().to_string(),
        device_id: uuid::Uuid::new_v4().simple().to_string(),
        device_token: random_hex(16),
    };
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    match Connection::connect(&params, false, tx).await {
        Ok(_) => Ok(()),
        Err(TransportError::Rejected(c) | TransportError::Relay(c)) if c == "group_not_found" || c == "ticket_required" => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// Join the group of the device named `alias` using its master password; the
/// host's user must approve the request (it shows `device_name`). Leaves the
/// merge pending (`bootstrap::PENDING_KEY`).
pub async fn join(db: &Db, alias: &str, password: String, device_name: &str) -> Result<SyncSecrets, String> {
    let (relay_url, auth_token) = relay_endpoint(db, None).await?;
    check_relay_auth(&relay_url, &auth_token).await?;
    let psk = tokio::task::spawn_blocking(move || crate::remote::login::derive_login_psk(&password))
        .await
        .map_err(|e| e.to_string())?;
    let code = crate::remote::login::login_code(Some(alias));
    let grant = request_grant(&relay_url, &code, &psk, device_name)
        .await
        .map_err(|e| e.to_string())?;

    let sync = SyncSecrets {
        group_id: grant.group_id,
        group_key: hex::encode(&grant.group_key),
        device_token: random_hex(32),
        device_id: uuid::Uuid::new_v4().simple().to_string(),
        relay_token: None,
    };
    let params = ConnectParams {
        relay_url,
        auth_token,
        group_id: sync.group_id.clone(),
        device_id: sync.device_id.clone(),
        device_token: sync.device_token.clone(),
    };
    enter_group(db, &sync, &params, false, Some(&grant.ticket)).await?;
    tracing::info!(event = "sync_group_joined");
    Ok(sync)
}

/// Leave the group: revoke this device on the relay (best effort), wipe the
/// Keychain entry and forget the group's device records locally. Returns
/// whether the relay accepted the revoke.
pub async fn leave(db: &Db) -> Result<bool, String> {
    let Some(sync) = crate::secrets::get_sync_secrets() else {
        return Ok(false);
    };
    let revoked = match connect_params(db, &sync).await {
        Ok(params) => revoke_self(&params).await,
        Err(_) => false,
    };
    crate::secrets::delete_sync_secrets().map_err(|e| e.to_string())?;
    if let Err(e) = forget_group_settings(db).await {
        tracing::warn!(event = "sync_leave_cleanup_failed", error = %e);
    }
    super::set_member(db, false).await?;
    super::bootstrap::clear_pending(db).await?;
    tracing::info!(event = "sync_group_left", revoked);
    Ok(revoked)
}

async fn revoke_self(params: &ConnectParams) -> bool {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    match Connection::connect(params, false, tx).await {
        // The relay may kick this connection before its "ok" arrives.
        Ok(conn) => matches!(
            conn.revoke(&params.device_id).await,
            Ok(()) | Err(TransportError::Rejected(_) | TransportError::Closed)
        ),
        // Already revoked: nothing left to do on the relay.
        Err(TransportError::Rejected(_)) => true,
        Err(_) => false,
    }
}

// ---- host side ---------------------------------------------------------------

/// A join request waiting for the user (`sync://join-request`).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct JoinRequest {
    pub request_id: String,
    pub device_name: String,
    /// Unix millis after which the request is denied unanswered.
    pub expires_at: i64,
}

/// Host-side notifications for the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum HostEvent {
    Request(JoinRequest),
    /// The request was answered, timed out or withdrawn; `granted` tells which.
    Resolved { request_id: String, granted: bool },
    /// The accept window opened (`Some` = expiry, unix millis) or closed.
    Window(Option<i64>),
}

#[derive(Default)]
struct Accept {
    /// Wall-clock expiry (unix millis) and its monotonic twin.
    until: Option<(i64, Instant)>,
    pending: Option<(String, oneshot::Sender<bool>)>,
}

static ACCEPT: Mutex<Accept> = Mutex::new(Accept { until: None, pending: None });

fn accept() -> std::sync::MutexGuard<'static, Accept> {
    ACCEPT.lock().unwrap_or_else(|p| p.into_inner())
}

/// Open (or extend) the accept window; returns its expiry (unix millis).
pub fn open_accept_window() -> i64 {
    let expires = chrono::Utc::now().timestamp_millis() + ACCEPT_WINDOW.as_millis() as i64;
    accept().until = Some((expires, Instant::now() + ACCEPT_WINDOW));
    tracing::info!(event = "sync_accept_window_opened");
    expires
}

/// Close the window; a request still waiting for an answer is denied.
pub fn close_accept_window() {
    let mut a = accept();
    a.until = None;
    a.pending = None;
}

/// The open window's expiry (unix millis), if any.
pub fn accept_window_expires() -> Option<i64> {
    let mut a = accept();
    match a.until {
        Some((ms, at)) if Instant::now() < at => Some(ms),
        Some(_) => {
            a.until = None;
            None
        }
        None => None,
    }
}

/// The user's answer to a pending request.
pub fn respond_join(request_id: &str, allow: bool) -> Result<(), String> {
    let mut a = accept();
    match a.pending.take() {
        Some((id, tx)) if id == request_id => {
            let _ = tx.send(allow);
            Ok(())
        }
        other => {
            a.pending = other;
            Err("that join request is no longer pending".into())
        }
    }
}

/// Whether an opened login-channel frame asks for a sync grant.
pub fn is_sync_join(plaintext: &[u8]) -> bool {
    serde_json::from_slice::<Value>(plaintext)
        .ok()
        .is_some_and(|v| v["kind"] == "cmd" && v["action"] == SYNC_JOIN_ACTION)
}

/// The requesting device's name from a `sync_join` command, made safe to show.
pub fn requested_name(plaintext: &[u8]) -> String {
    let v: Value = serde_json::from_slice(plaintext).unwrap_or_default();
    let name: String = v["device_name"]
        .as_str()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_DEVICE_NAME)
        .collect();
    let name = name.trim();
    if name.is_empty() { "unnamed device".to_string() } else { name.to_string() }
}

/// The host's verdict on one `sync_join`: the sealed reply plus audit fields.
#[derive(Debug, Clone)]
pub struct HostOutcome {
    pub reply: Value,
    pub granted: bool,
    /// Audit reason (no secrets): why it was denied, or who was let in.
    pub detail: String,
}

fn denied(reason: &str, name: &str) -> HostOutcome {
    HostOutcome {
        reply: json!({ "kind": "sync_denied", "reason": reason }),
        granted: false,
        detail: format!("{reason}: {name}"),
    }
}

/// Answer a password-proven `sync_join` (`proof` = the opened command).
/// `on_pending` runs once the request is waiting for the user, so the caller
/// can tell the joiner to keep waiting. Emits the host events on `app`.
pub async fn host_join(db: &Db, app: &tauri::AppHandle, proof: &[u8], on_pending: impl FnOnce()) -> HostOutcome {
    use tauri::Emitter;
    let sync = crate::secrets::get_sync_secrets();
    let params = match &sync {
        Some(s) => connect_params(db, s).await.ok(),
        None => None,
    };
    let emit = |e: HostEvent| {
        let _ = match e {
            HostEvent::Request(r) => app.emit(JOIN_REQUEST_EVENT, r),
            HostEvent::Resolved { request_id, granted } => {
                app.emit(JOIN_RESOLVED_EVENT, json!({ "request_id": request_id, "granted": granted }))
            }
            HostEvent::Window(w) => app.emit(ACCEPT_WINDOW_EVENT, json!({ "expires_at": w })),
        };
    };
    decide_join(sync.as_ref(), params, &requested_name(proof), emit, on_pending).await
}

/// [`host_join`] without the Keychain / app: `params` connects as this host
/// (to announce the ticket).
pub async fn decide_join(
    sync: Option<&SyncSecrets>,
    params: Option<ConnectParams>,
    name: &str,
    emit: impl Fn(HostEvent),
    on_pending: impl FnOnce(),
) -> HostOutcome {
    let Some(sync) = sync else { return denied("not_in_group", name) };
    let Some(remaining) = ({
        let a = accept();
        a.until.map(|(_, at)| at.saturating_duration_since(Instant::now()))
    })
    .filter(|d| !d.is_zero()) else {
        return denied("not_accepting", name);
    };
    let request_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = oneshot::channel();
    {
        let mut a = accept();
        if a.pending.as_ref().is_some_and(|(_, tx)| !tx.is_closed()) {
            return denied("busy", name);
        }
        a.pending = Some((request_id.clone(), tx));
    }
    let wait = remaining.min(APPROVAL_TIMEOUT);
    on_pending();
    emit(HostEvent::Request(JoinRequest {
        request_id: request_id.clone(),
        device_name: name.to_string(),
        expires_at: chrono::Utc::now().timestamp_millis() + wait.as_millis() as i64,
    }));
    let allowed = matches!(tokio::time::timeout(wait, rx).await, Ok(Ok(true)));
    {
        let mut a = accept();
        if a.pending.as_ref().is_some_and(|(id, _)| *id == request_id) {
            a.pending = None;
        }
    }
    if !allowed {
        emit(HostEvent::Resolved { request_id, granted: false });
        return denied("declined", name);
    }

    // Announce the ticket before handing it out.
    let ticket = random_hex(32);
    let invited = match params {
        Some(p) => {
            let (etx, _erx) = tokio::sync::mpsc::unbounded_channel();
            match Connection::connect(&p, false, etx).await {
                Ok(conn) => conn.sync_invite(&sha256_hex(&ticket), TICKET_TTL_SECS).await.map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        }
        None => Err("relay not configured".to_string()),
    };
    if let Err(e) = invited {
        tracing::warn!(event = "sync_invite_failed", error = %e);
        emit(HostEvent::Resolved { request_id, granted: false });
        return denied("relay_error", name);
    }
    // One device per window.
    close_accept_window();
    emit(HostEvent::Resolved { request_id, granted: true });
    emit(HostEvent::Window(None));
    HostOutcome {
        reply: grant_payload(sync, &ticket),
        granted: true,
        detail: format!("granted: {name}"),
    }
}

fn grant_payload(s: &SyncSecrets, ticket: &str) -> Value {
    json!({
        "kind": "sync_grant",
        "group_id": s.group_id,
        "group_key": s.group_key,
        "ticket": ticket,
    })
}

// ---- join client (Rust port of src/controller/login.ts) ----------------------

/// Run the password-login handshake on `login_code` and ask for a sync grant
/// as `device_name`. `psk` is the login PSK (`remote::login::derive_login_psk`).
pub async fn request_grant(
    relay_url: &str,
    login_code: &str,
    psk: &[u8; 32],
    device_name: &str,
) -> Result<Grant, JoinError> {
    let dial = tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(relay_url));
    let ws = match dial.await {
        Ok(Ok((ws, _))) => ws,
        Ok(Err(e)) => return Err(JoinError::Network(e.to_string())),
        Err(_) => return Err(JoinError::Network("connect timeout".into())),
    };
    let (mut sink, mut stream) = ws.split();
    let send = |v: Value| Message::Text(v.to_string());
    sink.send(send(json!({ "t": "join", "pair_code": login_code })))
        .await
        .map_err(|e| JoinError::Network(e.to_string()))?;

    let mut room_id: Option<String> = None;
    let mut session: Option<Session> = None;
    let mut pending = false;
    let mut deadline = Instant::now() + LOGIN_TIMEOUT;
    let mut seq = 0u64;
    let result = loop {
        let msg = match tokio::time::timeout_at(deadline, stream.next()).await {
            Ok(Some(m)) => m,
            Ok(None) => break Err(JoinError::Network("connection closed".into())),
            // Silence after our sealed frame = the host could not open it;
            // after "pending" = nobody answered on the host.
            Err(_) => {
                break Err(if pending {
                    JoinError::Declined
                } else if session.is_some() {
                    JoinError::WrongPassword
                } else {
                    JoinError::HostOffline
                })
            }
        };
        let text = match msg {
            Ok(Message::Text(t)) => t,
            Ok(_) => continue,
            Err(e) => break Err(JoinError::Network(e.to_string())),
        };
        let Ok(env) = serde_json::from_str::<Envelope>(&text) else { continue };
        match env.t {
            FrameType::RoomReady if !env.room_id.is_empty() => {
                room_id = Some(env.room_id);
                continue;
            }
            FrameType::Error if matches!(env.code.as_deref(), Some("pair_invalid" | "room_not_found")) => {
                break Err(JoinError::HostOffline);
            }
            FrameType::PeerLeft => {
                break Err(if pending {
                    JoinError::Declined
                } else if session.is_some() {
                    JoinError::WrongPassword
                } else {
                    JoinError::HostOffline
                });
            }
            _ => {}
        }
        let Some(room) = room_id.clone().filter(|r| *r == env.room_id) else { continue };
        let Some(cipher) = env.cipher else { continue };
        match env.t {
            FrameType::Control if session.is_none() => {
                let Some(json) = B64.decode(&cipher).ok() else { continue };
                let Ok(HandshakePayload::HsOffer { host_pub, host_fingerprint: fp }) =
                    serde_json::from_slice::<HandshakePayload>(&json)
                else {
                    continue;
                };
                let Some(host_pub) = crate::remote::pairing_unhex(&host_pub).filter(|b| b.len() == 32) else {
                    continue;
                };
                // No pinned fingerprint on this path (the password
                // authenticates the host), but the offer must be self-consistent.
                if host_fingerprint(&host_pub).ok().as_deref() != Some(fp.as_str()) {
                    continue;
                }
                let keypair = Keypair::generate();
                let answer = HandshakePayload::HsAnswer {
                    controller_pub: crate::remote::pairing_hex(&keypair.public),
                };
                let answer = B64.encode(serde_json::to_vec(&answer).unwrap_or_default());
                seq += 1;
                if let Err(e) = sink.send(send(json!({ "t": "control", "room_id": room, "cipher": answer, "seq": seq }))).await {
                    break Err(JoinError::Network(e.to_string()));
                }
                let s = match Session::new(psk, &keypair, &host_pub) {
                    Ok(s) => s,
                    Err(e) => break Err(JoinError::Other(e.to_string())),
                };
                let cmd = json!({ "kind": "cmd", "action": SYNC_JOIN_ACTION, "device_name": device_name }).to_string();
                seq += 1;
                if let Err(e) = sink
                    .send(send(json!({ "t": "cmd", "room_id": room, "cipher": s.seal(cmd.as_bytes()), "seq": seq })))
                    .await
                {
                    break Err(JoinError::Network(e.to_string()));
                }
                session = Some(s);
            }
            FrameType::Stream => {
                let Some(s) = session.as_ref() else { continue };
                let Ok(pt) = s.open(&cipher) else { continue };
                let Ok(v) = serde_json::from_slice::<Value>(&pt) else { continue };
                match v["kind"].as_str() {
                    Some("sync_pending") => {
                        pending = true;
                        deadline = Instant::now() + PENDING_TIMEOUT;
                    }
                    Some("sync_grant") => break parse_grant(&v),
                    Some("sync_denied") => break Err(denial(v["reason"].as_str().unwrap_or_default())),
                    Some("login_granted") => break Err(JoinError::HostUnsupported),
                    _ => {}
                }
            }
            _ => {}
        }
    };
    let _ = sink.close().await;
    result
}

fn denial(reason: &str) -> JoinError {
    match reason {
        "not_in_group" => JoinError::HostNotInGroup,
        "not_accepting" => JoinError::NotAccepting,
        "declined" => JoinError::Declined,
        "busy" => JoinError::Busy,
        "relay_error" => JoinError::Other("relay_error: that machine could not reach the relay".into()),
        other => JoinError::Other(format!("denied: {other}")),
    }
}

fn parse_grant(v: &Value) -> Result<Grant, JoinError> {
    let bad = || JoinError::Other("malformed sync grant".into());
    let group_id = v["group_id"].as_str().filter(|s| !s.is_empty()).ok_or_else(bad)?;
    let group_key = v["group_key"]
        .as_str()
        .and_then(|k| hex::decode(k).ok())
        .filter(|k| k.len() == KEY_LEN)
        .ok_or_else(bad)?;
    // A grant without a ticket comes from a build that hands out the relay
    // token instead; the relay no longer registers devices that way.
    let ticket = v["ticket"].as_str().filter(|t| !t.is_empty()).ok_or(JoinError::HostUnsupported)?;
    Ok(Grant {
        group_id: group_id.to_string(),
        group_key,
        ticket: ticket.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::test_relay::TestRelay;
    use tokio::net::TcpListener;

    fn secrets() -> SyncSecrets {
        SyncSecrets {
            group_id: "g-1".into(),
            group_key: hex::encode([5u8; 32]),
            device_token: "tok".into(),
            device_id: "dev-host".into(),
            relay_token: Some("relay-tok".into()),
        }
    }

    #[test]
    fn grant_carries_group_and_ticket_never_tokens() {
        let v = grant_payload(&secrets(), "tkt");
        assert_eq!(v["kind"], "sync_grant");
        assert_eq!(v["group_id"], "g-1");
        let text = v.to_string();
        assert!(!text.contains("relay-tok") && v.get("relay_token").is_none(), "relay host token must stay private");
        assert!(!text.contains("\"tok\""), "own device token must stay private");
        let g = parse_grant(&v).unwrap();
        assert_eq!(g.group_key, vec![5u8; 32]);
        assert_eq!(g.ticket, "tkt");
    }

    #[test]
    fn recognises_the_join_command_and_cleans_the_name() {
        assert!(is_sync_join(br#"{"kind":"cmd","action":"sync_join"}"#));
        assert!(!is_sync_join(br#"{"kind":"cmd","action":"login"}"#));
        assert!(!is_sync_join(b"garbage"));
        assert_eq!(requested_name(br#"{"device_name":"  Mac\u0007 mini "}"#), "Mac mini");
        assert_eq!(requested_name(br#"{"action":"sync_join"}"#), "unnamed device");
        assert_eq!(requested_name(format!(r#"{{"device_name":"{}"}}"#, "x".repeat(200)).as_bytes()).len(), MAX_DEVICE_NAME);
    }

    #[test]
    fn rejects_malformed_and_ticketless_grants() {
        assert!(parse_grant(&json!({ "kind": "sync_grant", "group_id": "g", "group_key": "abcd", "ticket": "t" })).is_err());
        assert!(parse_grant(&json!({ "kind": "sync_grant", "group_key": hex::encode([1u8; 32]), "ticket": "t" })).is_err());
        // An old host hands out its relay token instead of a ticket.
        let old = json!({ "kind": "sync_grant", "group_id": "g", "group_key": hex::encode([1u8; 32]), "relay_token": "r" });
        assert_eq!(parse_grant(&old).err(), Some(JoinError::HostUnsupported));
    }

    /// A fake relay + host on one socket, speaking the host side of the
    /// password-login flow exactly like `remote::agent` (offer on join, answer,
    /// open the first sealed cmd with the login psk, reply sealed).
    async fn fake_host(host_psk: [u8; 32], replies: Vec<Value>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let (mut sink, mut stream) = ws.split();
            let kp = Keypair::generate();
            let mut controller_pub = None;
            while let Some(Ok(Message::Text(t))) = stream.next().await {
                let v: Value = serde_json::from_str(&t).unwrap();
                let out = match v["t"].as_str().unwrap() {
                    "join" => {
                        assert_eq!(v["pair_code"], "devdy-login-v1/mini");
                        let offer = HandshakePayload::HsOffer {
                            host_pub: crate::remote::pairing_hex(&kp.public),
                            host_fingerprint: host_fingerprint(&kp.public).unwrap(),
                        };
                        let c = B64.encode(serde_json::to_vec(&offer).unwrap());
                        vec![
                            json!({ "t": "room_ready", "room_id": "rm1" }),
                            json!({ "t": "control", "room_id": "rm1", "cipher": c, "seq": 1 }),
                        ]
                    }
                    "control" => {
                        let raw = B64.decode(v["cipher"].as_str().unwrap()).unwrap();
                        if let HandshakePayload::HsAnswer { controller_pub: p } = serde_json::from_slice(&raw).unwrap() {
                            controller_pub = crate::remote::pairing_unhex(&p);
                        }
                        vec![]
                    }
                    "cmd" => {
                        let s = Session::new(&host_psk, &kp, controller_pub.as_ref().unwrap()).unwrap();
                        match s.open(v["cipher"].as_str().unwrap()) {
                            Ok(pt) => {
                                assert!(is_sync_join(&pt));
                                assert_eq!(requested_name(&pt), "Air");
                                replies
                                    .iter()
                                    .map(|r| json!({ "t": "stream", "room_id": "rm1", "cipher": s.seal(r.to_string().as_bytes()), "seq": 2 }))
                                    .collect()
                            }
                            // Wrong password: the host revokes the room.
                            Err(_) => vec![json!({ "t": "peer_left", "room_id": "rm1" })],
                        }
                    }
                    _ => vec![],
                };
                for o in out {
                    sink.send(Message::Text(o.to_string())).await.unwrap();
                }
            }
        });
        format!("ws://{addr}/ws")
    }

    #[tokio::test]
    async fn join_flow_waits_for_approval_then_receives_the_grant() {
        let psk = [9u8; 32];
        let replies = vec![json!({ "kind": "sync_pending" }), grant_payload(&secrets(), "tkt")];
        let url = fake_host(psk, replies).await;
        let g = request_grant(&url, "devdy-login-v1/mini", &psk, "Air").await.unwrap();
        assert_eq!(g.group_id, "g-1");
        assert_eq!(g.group_key, vec![5u8; 32]);
        assert_eq!(g.ticket, "tkt");
    }

    #[tokio::test]
    async fn join_flow_wrong_password() {
        let url = fake_host([9u8; 32], vec![]).await;
        let err = request_grant(&url, "devdy-login-v1/mini", &[1u8; 32], "Air").await.err();
        assert_eq!(err, Some(JoinError::WrongPassword));
    }

    #[tokio::test]
    async fn join_flow_maps_denials() {
        let psk = [9u8; 32];
        for (reason, want) in [
            ("not_in_group", JoinError::HostNotInGroup),
            ("not_accepting", JoinError::NotAccepting),
            ("declined", JoinError::Declined),
            ("busy", JoinError::Busy),
        ] {
            let url = fake_host(psk, vec![json!({ "kind": "sync_denied", "reason": reason })]).await;
            let err = request_grant(&url, "devdy-login-v1/mini", &psk, "Air").await.err();
            assert_eq!(err, Some(want), "{reason}");
        }
    }

    fn params(relay: &TestRelay, group: &str, device: &str) -> ConnectParams {
        ConnectParams {
            relay_url: relay.url(),
            auth_token: TestRelay::TOKEN.into(),
            group_id: group.into(),
            device_id: device.into(),
            device_token: format!("tok-{device}"),
        }
    }

    async fn connect(p: &ConnectParams, create: bool, ticket: Option<&str>) -> Result<Connection, TransportError> {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        Connection::connect_with_ticket(p, create, ticket, tx).await
    }

    /// Every accept-window scenario in one test: the window is process-global.
    #[tokio::test]
    async fn host_grants_only_inside_the_window_and_after_approval() {
        let relay = TestRelay::start().await;
        let host = params(&relay, "g-1", "dev-host");
        connect(&host, true, None).await.unwrap();
        let sync = secrets();
        let quiet = |_: HostEvent| {};

        close_accept_window();
        let out = decide_join(Some(&sync), Some(host.clone()), "Air", quiet, || {}).await;
        assert!(!out.granted);
        assert_eq!(out.reply["reason"], "not_accepting");
        assert_eq!(decide_join(None, None, "Air", quiet, || {}).await.reply["reason"], "not_in_group");

        // Declined by the user.
        open_accept_window();
        let answer = |allow: bool| {
            move |e: HostEvent| {
                if let HostEvent::Request(r) = e {
                    assert_eq!(r.device_name, "Air");
                    respond_join(&r.request_id, allow).unwrap();
                }
            }
        };
        let mut told = false;
        let out = decide_join(Some(&sync), Some(host.clone()), "Air", answer(false), || told = true).await;
        assert!(told, "the joiner is told to wait");
        assert_eq!(out.reply["reason"], "declined");
        assert!(out.detail.contains("Air"));
        assert!(accept_window_expires().is_some(), "a refusal keeps the window open");

        // Approved: the ticket is announced first and works exactly once.
        let out = decide_join(Some(&sync), Some(host.clone()), "Air", answer(true), || {}).await;
        assert!(out.granted, "{:?}", out.reply);
        assert!(out.reply.get("relay_token").is_none());
        assert!(accept_window_expires().is_none(), "one device per window");
        let ticket = parse_grant(&out.reply).unwrap().ticket;
        connect(&params(&relay, "g-1", "dev-air"), false, Some(&ticket)).await.unwrap();
        let reuse = connect(&params(&relay, "g-1", "dev-other"), false, Some(&ticket)).await.err();
        assert_eq!(reuse, Some(TransportError::Rejected("ticket_invalid".into())));
        let none = connect(&params(&relay, "g-1", "dev-other"), false, None).await.err();
        assert_eq!(none, Some(TransportError::Rejected("ticket_required".into())));

        // Closing the window denies a request still waiting for an answer.
        open_accept_window();
        let closer = |e: HostEvent| {
            if let HostEvent::Request(_) = e {
                close_accept_window();
            }
        };
        let out = decide_join(Some(&sync), Some(host.clone()), "Air", closer, || {}).await;
        assert_eq!(out.reply["reason"], "declined");
        assert!(respond_join("nope", true).is_err());
    }

    async fn mem_db() -> Db {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&db).await.unwrap();
        crate::sync::install(&db).await.unwrap();
        db
    }

    #[tokio::test]
    async fn new_membership_device_keeps_hlcs_and_pending_ops_valid() {
        use crate::sync::clock::Hlc;
        use crate::sync::store::{SqliteSyncStore, SyncStore};
        let db = mem_db().await;
        let old = crate::sync::ensure_device(&db).await.unwrap();
        sqlx::query("INSERT INTO projects (id, name, path, created_at) VALUES ('p1', 'P', '/nonexistent/p1', 'x')")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO runs (id, project_id, type, status, engine, created_at, owner_device) VALUES ('r1', 'p1', 'session', 'done', 'claude', 'x', ?)")
            .bind(&old)
            .execute(&db)
            .await
            .unwrap();
        crate::sync::leader::touch_self(&db, &old, "MacBook").await.unwrap();
        // A row seen from the group under the old id, far in the future.
        let far = Hlc::new(9_000_000_000_000, 3, "zzz");
        let store = SqliteSyncStore::new(db.clone(), old.clone());
        let op = crate::sync::op::Op {
            v: crate::sync::op::OP_VERSION,
            schema: crate::sync::SCHEMA_VERSION,
            hlc: far.clone(),
            tbl: "todos".into(),
            pk: json!("t1"),
            kind: crate::sync::op::OpKind::Upsert,
            row: json!({"id": "t1", "text": "x", "done": 0, "position": 0, "created_at": "x"}).as_object().cloned(),
        };
        store.apply_remote(vec![op]).await.unwrap();
        sqlx::query("UPDATE todos SET text = 'local edit' WHERE id = 't1'").execute(&db).await.unwrap();

        sqlx::query("DELETE FROM sync_outbox WHERE tbl != 'todos'").execute(&db).await.unwrap();
        let new = "new-dev".to_string();
        new_membership_device(&db, &new).await.unwrap();
        // N3: the id switch itself is not pushed (it would overwrite a peer's take-over).
        let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_outbox WHERE tbl != 'todos'").fetch_one(&db).await.unwrap();
        assert_eq!(queued, 0);
        assert_eq!(crate::sync::leader::my_device_id(&db).await.as_deref(), Some(new.as_str()));
        let owner: String = sqlx::query_scalar("SELECT owner_device FROM runs WHERE id = 'r1'").fetch_one(&db).await.unwrap();
        assert_eq!(owner, new, "own runs stay own");
        assert!(crate::sync::leader::record(&db, &old).await.is_none(), "old device record dropped");

        // The edit queued before the switch still goes out, ordered after
        // everything observed, stamped with the new id.
        let store = SqliteSyncStore::new(db.clone(), new.clone());
        let ops = store.pending(100).await.unwrap();
        let todo = ops.iter().find(|(_, o)| o.tbl == "todos").expect("pending todo op");
        assert!(todo.1.hlc > far);
        assert_eq!(todo.1.hlc.device, new);

        // A peer sends the run back still owned by the old id: it stays ours,
        // without echoing the rewrite.
        store.mark_pushed(ops.last().unwrap().0).await.unwrap();
        let back = crate::sync::op::Op {
            v: crate::sync::op::OP_VERSION,
            schema: crate::sync::SCHEMA_VERSION,
            hlc: Hlc::new(9_000_000_100_000, 0, "peer"),
            tbl: "runs".into(),
            pk: json!("r1"),
            kind: crate::sync::op::OpKind::Upsert,
            row: json!({"id": "r1", "project_id": "p1", "type": "session", "status": "done", "engine": "claude", "created_at": "x", "owner_device": old, "title": "renamed"}).as_object().cloned(),
        };
        let r = store.apply_remote(vec![back]).await.unwrap();
        assert_eq!(r.applied, 1, "{r:?}");
        let (owner, title): (String, Option<String>) =
            sqlx::query_as("SELECT owner_device, title FROM runs WHERE id = 'r1'").fetch_one(&db).await.unwrap();
        assert_eq!((owner.as_str(), title.as_deref()), (new.as_str(), Some("renamed")));
        assert!(store.pending(100).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn relay_auth_is_checked_before_asking_the_host() {
        let relay = TestRelay::start().await;
        assert_eq!(check_relay_auth(&relay.url(), TestRelay::TOKEN).await, Ok(()));
        let bad = check_relay_auth(&relay.url(), "wrong").await.unwrap_err();
        assert!(bad.contains("auth_failed"), "{bad}");
        assert!(check_relay_auth("ws://127.0.0.1:1/ws", TestRelay::TOKEN).await.is_err());
    }

    #[tokio::test]
    async fn leaving_forgets_the_group_device_records_without_pushing() {
        let db = mem_db().await;
        crate::sync::leader::touch_self(&db, "me", "MacBook").await.unwrap();
        crate::sync::leader::touch_self(&db, "other", "Mini").await.unwrap();
        crate::sync::leader::set_leader(&db, Some("other")).await.unwrap();
        sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES ('theme', 'dark')").execute(&db).await.unwrap();
        forget_group_settings(&db).await.unwrap();
        let left: Vec<String> = sqlx::query_scalar("SELECT key FROM settings WHERE key LIKE 'sync_%'").fetch_all(&db).await.unwrap();
        assert!(left.is_empty(), "{left:?}");
        let queued: Vec<String> = sqlx::query_scalar("SELECT pk FROM sync_outbox WHERE tbl = 'settings'").fetch_all(&db).await.unwrap();
        assert_eq!(queued, vec!["\"theme\"".to_string()], "no delete pushed, nothing left queued for the old group");
    }

    #[tokio::test]
    async fn leave_then_rejoin_with_a_fresh_device_id() {
        let db = mem_db().await;
        let relay = TestRelay::start().await;
        let host = params(&relay, "g", "host");
        let host_conn = connect(&host, true, None).await.unwrap();

        let id1 = "id-one".to_string();
        new_membership_device(&db, &id1).await.unwrap();
        host_conn.sync_invite(&sha256_hex("t1"), 60).await.unwrap();
        let me1 = params(&relay, "g", &id1);
        connect(&me1, false, Some("t1")).await.unwrap();

        // Leave: revoke self; that id can never come back.
        assert!(revoke_self(&me1).await);
        let again = connect(&me1, false, None).await.err();
        assert_eq!(again, Some(TransportError::Rejected("device_revoked".into())));

        // Rejoin: a new membership, a new id, a new ticket.
        let id2 = "id-two".to_string();
        new_membership_device(&db, &id2).await.unwrap();
        assert_ne!(id1, id2);
        host_conn.sync_invite(&sha256_hex("t2"), 60).await.unwrap();
        connect(&params(&relay, "g", &id2), false, Some("t2")).await.unwrap();

        // A non-creator may not remove another device; the creator may.
        let mine = connect(&params(&relay, "g", &id2), false, None).await.unwrap();
        assert_eq!(mine.revoke("host").await.err(), Some(TransportError::Rejected("forbidden".into())));
        host_conn.revoke(&id2).await.unwrap();
    }
}
