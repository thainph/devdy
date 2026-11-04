//! Remote Control (C1) — Host agent (U3).
//!
//! Backend that lets a Controller device drive a running Devdy session over a
//! self-hosted Cloud Relay, end-to-end encrypted (SRS-RC-C1). This module owns:
//!   - the outbound WSS client to the relay ([`agent`]),
//!   - the per-session bound model + link/OTP/session-secret generation ([`session`]),
//!   - the in-process event tap the run drain publishes to ([`bus`]),
//!   - the prioritised outbound queue to the relay ([`outbound`]),
//!   - sealing + forwarding run events and history replay ([`forwarder`]),
//!   - the security-gated command handler ([`handler`]) built on the pure rules
//!     in [`command`],
//!   - the append-only audit ([`audit`]),
//!   - the wire protocol ([`protocol`]),
//!   - the Tauri commands the Host UI (U5) calls ([`commands`]).
//!
//! Secrets never touch SQLite or logs: the relay `auth_token` and per-device
//! `psk` live in the Keychain (`crate::secrets`). The relay only ever sees
//! ciphertext + `room_id` (SEC-006 / CON-04).

pub mod agent;
pub mod audit;
pub mod bus;
pub mod command;
pub mod commands;
pub mod data;
pub mod focus;
pub mod forwarder;
pub mod handler;
pub mod login;
pub mod meta;
pub mod outbound;
pub mod protocol;
pub mod push;
pub mod session;

pub use bus::{RemoteBus, RemoteRunEvent};
pub use meta::{apply_run_meta, RunMetaStore};

use crate::db::Db;
use crate::runs::{BrokerApprovals, RunRegistry};
use session::BoundSession;
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tauri::AppHandle;
use tokio::sync::Mutex as TokioMutex;

/// Runs whose live transcript the Controller is currently streaming.
///
/// Shared between the command handler (which mutates it on `open_run` /
/// `close_run`) and the forwarder (which reads it per event), so navigation
/// takes effect on the very next event rather than at the next reconnect.
pub type RunSubscriptions = Arc<TokioMutex<HashSet<String>>>;

/// The run the Controller is currently looking at. Read by `remote_status` so
/// the desktop can show where the phone is; `None` before authentication.
pub type FocusRun = Arc<TokioMutex<Option<String>>>;

/// Persisted-config settings keys (DATA-001). `relay_url` + `enabled` live in
/// the `settings` KV; the `device_id` is generated once and also kept there.
pub const KEY_RELAY_URL: &str = "remote_relay_url";
pub const KEY_ENABLED: &str = "remote_enabled";
pub const KEY_DEVICE_ID: &str = "remote_device_id";
/// Optional short name of this device; selects its password-login code so
/// several Devdy hosts can share one relay (see `login::login_code`).
pub const KEY_DEVICE_ALIAS: &str = "remote_device_alias";

/// Tauri event carrying the current [`DeviceConflict`] (`null` once cleared).
pub const DEVICE_CONFLICT_EVENT: &str = "remote://device-conflict";

/// A clash the relay reported for this Host. Serialized camelCase:
/// `{ "kind": "device_id" }` or `{ "kind": "alias", "alias": "Mac mini" }`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceConflict {
    /// `device_id`: another running Devdy is connected with this device id, so
    /// the relay refused this one (Remote Control and device sync are off
    /// here until it is resolved). `alias`: another online device is listed
    /// under the same name (a warning only).
    pub kind: String,
    /// The clashing name, for `alias`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
}

impl DeviceConflict {
    pub fn device_id() -> Self {
        Self {
            kind: "device_id".to_string(),
            alias: None,
        }
    }

    pub fn alias(alias: &str) -> Self {
        Self {
            kind: "alias".to_string(),
            alias: Some(alias.to_string()),
        }
    }
}

/// Shared slot for the current conflict.
pub type ConflictSlot = Arc<std::sync::Mutex<Option<DeviceConflict>>>;

/// Store `conflict` and tell the UI when it changed.
pub fn set_device_conflict(app: &AppHandle, slot: &ConflictSlot, conflict: Option<DeviceConflict>) {
    use tauri::Emitter;
    let changed = match slot.lock() {
        Ok(mut current) if *current != conflict => {
            *current = conflict.clone();
            true
        }
        _ => false,
    };
    if changed {
        let _ = app.emit(DEVICE_CONFLICT_EVENT, conflict);
    }
}

/// Why the agent is not connected to the relay right now, as a stable code the
/// UI maps to a message (`remote_status.last_error`). Cleared once the relay
/// accepts `register_host`, and when the agent stops.
pub mod last_error {
    /// DNS / TCP / TLS failure or no answer in time.
    pub const RELAY_UNREACHABLE: &str = "relay_unreachable";
    /// The URL is malformed, or the server answered but is not a relay
    /// WebSocket endpoint (HTTP status instead of an upgrade).
    pub const RELAY_BAD_URL: &str = "relay_bad_url";
    /// The relay refused the host auth token.
    pub const RELAY_AUTH_FAILED: &str = "relay_auth_failed";
    /// The relay rejected a frame it does not understand (an older relay).
    pub const RELAY_UNSUPPORTED: &str = "relay_unsupported";
    /// The relay throttled this Host.
    pub const RELAY_RATE_LIMITED: &str = "relay_rate_limited";
    /// Another running Devdy is connected with this device id.
    pub const DEVICE_ID_CONFLICT: &str = "device_id_conflict";
    /// Enabled, but the relay URL is missing.
    pub const RELAY_NOT_CONFIGURED: &str = "relay_not_configured";
    /// Enabled, but the relay auth token is missing from the Keychain.
    pub const TOKEN_MISSING: &str = "token_missing";

    /// The code for a failed dial.
    pub fn for_dial_error(e: &tokio_tungstenite::tungstenite::Error) -> &'static str {
        use tokio_tungstenite::tungstenite::Error;
        match e {
            Error::Url(_) | Error::Http(_) | Error::HttpFormat(_) => RELAY_BAD_URL,
            _ => RELAY_UNREACHABLE,
        }
    }

    /// The code for an `error` frame from the relay, `None` when that error
    /// does not stop this Host from serving.
    pub fn for_relay_error(code: Option<&str>) -> Option<&'static str> {
        match code? {
            "auth_failed" | "forbidden" => Some(RELAY_AUTH_FAILED),
            "bad_request" => Some(RELAY_UNSUPPORTED),
            "rate_limited" => Some(RELAY_RATE_LIMITED),
            "device_id_conflict" => Some(DEVICE_ID_CONFLICT),
            _ => None,
        }
    }
}

/// Shared slot for [`last_error`].
pub type ErrorSlot = Arc<std::sync::Mutex<Option<&'static str>>>;

pub fn set_last_error(slot: &ErrorSlot, code: Option<&'static str>) {
    if let Ok(mut current) = slot.lock() {
        *current = code;
    }
}

/// Random id of this app launch, sent on `register_host` (see
/// `protocol::Envelope::instance_id`). Never persisted.
pub fn instance_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

/// Live control handle for the running agent task.
struct AgentControl {
    stop: tokio::sync::watch::Sender<bool>,
    handle: tokio::task::JoinHandle<()>,
    /// To tell the UI on stop (the conflict it showed is cleared).
    app: AppHandle,
}

/// App-wide Remote Control state (Tauri managed).
#[derive(Clone)]
pub struct RemoteState {
    pub bus: RemoteBus,
    /// The single bound remote session (one run/room at a time). `None` when no
    /// remote control is active.
    pub bound: Arc<TokioMutex<Option<BoundSession>>>,
    pub connected: Arc<AtomicBool>,
    /// True while the agent's host↔relay socket is down and it is retrying with
    /// backoff (as opposed to a clean idle wait, where the socket is up). Lets the
    /// desktop badge show a truthful "reconnecting…" instead of a bare
    /// "disconnected" during a transient network blip.
    pub reconnecting: Arc<AtomicBool>,
    /// Signalled whenever the bound session changes (new link or ended) so a
    /// running agent re-syncs its relay listen without waiting for a reconnect.
    pub announce_signal: Arc<tokio::sync::Notify>,
    /// Signalled by the owner's kill switch: the agent revokes the current
    /// Controller room (it may re-join; ending the session blocks it).
    pub kick_signal: Arc<tokio::sync::Notify>,
    /// Runs the Controller is streaming right now. Lives here (not in the agent
    /// task) so `remote_status` can report it to the desktop UI.
    pub subscriptions: RunSubscriptions,
    /// The run the Controller is currently viewing.
    pub focus_run_id: FocusRun,
    /// Web Push to subscribed phones; its dispatcher runs with the agent.
    pub push: push::PushHub,
    /// The device id / alias clash the relay last reported, if any.
    pub device_conflict: ConflictSlot,
    /// Why the agent is not connected right now (see [`last_error`]).
    pub last_error: ErrorSlot,
    agent: Arc<TokioMutex<Option<AgentControl>>>,
}

impl Default for RemoteState {
    fn default() -> Self {
        Self::new()
    }
}

impl RemoteState {
    pub fn new() -> Self {
        RemoteState {
            bus: RemoteBus::new(),
            bound: Arc::new(TokioMutex::new(None)),
            connected: Arc::new(AtomicBool::new(false)),
            reconnecting: Arc::new(AtomicBool::new(false)),
            announce_signal: Arc::new(tokio::sync::Notify::new()),
            kick_signal: Arc::new(tokio::sync::Notify::new()),
            subscriptions: Arc::new(TokioMutex::new(HashSet::new())),
            focus_run_id: Arc::new(TokioMutex::new(None)),
            push: push::PushHub::default(),
            device_conflict: Arc::new(std::sync::Mutex::new(None)),
            last_error: Arc::new(std::sync::Mutex::new(None)),
            agent: Arc::new(TokioMutex::new(None)),
        }
    }

    /// Whether the agent task is currently running (not the socket state).
    pub async fn is_running(&self) -> bool {
        self.agent.lock().await.is_some()
    }

    /// Start the agent if not already running. Requires a configured relay URL
    /// and an auth_token in the keyring (SEC-001) — otherwise a no-op error.
    pub async fn start(
        &self,
        app: &AppHandle,
        db: &Db,
        registry: &RunRegistry,
        broker_approvals: &BrokerApprovals,
    ) -> Result<(), String> {
        if self.is_running().await {
            return Ok(());
        }
        let relay_url = get_setting(db, KEY_RELAY_URL).await.unwrap_or_default();
        if relay_url.trim().is_empty() {
            set_last_error(&self.last_error, Some(last_error::RELAY_NOT_CONFIGURED));
            return Err("relay URL is not configured".to_string());
        }
        let Some(auth_token) = crate::secrets::get_remote_auth_token() else {
            set_last_error(&self.last_error, Some(last_error::TOKEN_MISSING));
            return Err("relay auth token is not set (enable requires a token)".to_string());
        };
        set_last_error(&self.last_error, None);
        let device_id = ensure_device_id(db).await?;

        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let deps = agent::AgentDeps {
            app: app.clone(),
            db: db.clone(),
            registry: registry.clone(),
            broker_approvals: broker_approvals.clone(),
            bus: self.bus.clone(),
            bound: self.bound.clone(),
            connected: self.connected.clone(),
            reconnecting: self.reconnecting.clone(),
            announce_signal: self.announce_signal.clone(),
            kick_signal: self.kick_signal.clone(),
            subscriptions: self.subscriptions.clone(),
            focus_run_id: self.focus_run_id.clone(),
            device_conflict: self.device_conflict.clone(),
            last_error: self.last_error.clone(),
        };
        let config = agent::AgentConfig {
            relay_url,
            device_id,
        };
        let handle = tokio::spawn(agent::run_agent(config, deps, auth_token, stop_rx));
        self.push.start(db.clone(), self.bus.clone());
        *self.agent.lock().await = Some(AgentControl {
            stop: stop_tx,
            handle,
            app: app.clone(),
        });
        Ok(())
    }

    /// Stop the agent task if running (signals its reconnect loop to exit).
    pub async fn stop(&self) {
        let app = match self.agent.lock().await.take() {
            Some(ctrl) => {
                let _ = ctrl.stop.send(true);
                ctrl.handle.abort();
                Some(ctrl.app)
            }
            None => None,
        };
        self.push.stop();
        // Only meaningful while connected to the relay (and only ever set by a
        // running agent, so there is an app to tell).
        match &app {
            Some(app) => set_device_conflict(app, &self.device_conflict, None),
            None => {
                if let Ok(mut c) = self.device_conflict.lock() {
                    *c = None;
                }
            }
        }
        set_last_error(&self.last_error, None);
        self.connected
            .store(false, std::sync::atomic::Ordering::SeqCst);
        self.reconnecting
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Lowercase hex encode (shared by agent/pairing for pubkey exchange).
pub fn pairing_hex(bytes: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 0x0f) as usize] as char);
    }
    s
}

/// Decode a lowercase/uppercase hex string; `None` on any invalid character.
pub fn pairing_unhex(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    if !b.len().is_multiple_of(2) {
        return None;
    }
    let val = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    let mut out = Vec::with_capacity(b.len() / 2);
    for pair in b.chunks_exact(2) {
        out.push((val(pair[0])? << 4) | val(pair[1])?);
    }
    Some(out)
}

/// Read a `settings` value.
pub(crate) async fn get_setting(db: &Db, key: &str) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .filter(|v| !v.trim().is_empty())
}

/// Write a `settings` value.
pub(crate) async fn set_setting(db: &Db, key: &str, value: &str) -> Result<(), String> {
    sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)")
        .bind(key)
        .bind(value)
        .execute(db)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// The configured device alias, when set and valid.
pub(crate) async fn device_alias(db: &Db) -> Option<String> {
    get_setting(db, KEY_DEVICE_ALIAS)
        .await
        .filter(|a| login::valid_alias(a))
}

/// Replace the Host's `device_id` with a fresh one (after a `device_id`
/// conflict: a copied database on another machine). Returns the new id.
pub(crate) async fn regenerate_device_id(db: &Db) -> Result<String, String> {
    let id = uuid::Uuid::new_v4().to_string();
    set_setting(db, KEY_DEVICE_ID, &id).await?;
    Ok(id)
}

/// Return the Host's stable `device_id`, generating + persisting one on first
/// use (DATA-001).
pub(crate) async fn ensure_device_id(db: &Db) -> Result<String, String> {
    if let Some(id) = get_setting(db, KEY_DEVICE_ID).await {
        return Ok(id);
    }
    let id = uuid::Uuid::new_v4().to_string();
    set_setting(db, KEY_DEVICE_ID, &id).await?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_errors_map_to_stable_codes() {
        assert_eq!(last_error::for_relay_error(Some("auth_failed")), Some("relay_auth_failed"));
        assert_eq!(last_error::for_relay_error(Some("forbidden")), Some("relay_auth_failed"));
        assert_eq!(last_error::for_relay_error(Some("bad_request")), Some("relay_unsupported"));
        assert_eq!(last_error::for_relay_error(Some("rate_limited")), Some("relay_rate_limited"));
        assert_eq!(
            last_error::for_relay_error(Some("device_id_conflict")),
            Some("device_id_conflict")
        );
        // Room-level errors don't stop the Host from serving.
        assert_eq!(last_error::for_relay_error(Some("pair_invalid")), None);
        assert_eq!(last_error::for_relay_error(Some("alias_conflict")), None);
        assert_eq!(last_error::for_relay_error(None), None);
    }

    #[test]
    fn dial_errors_tell_a_bad_url_from_an_unreachable_relay() {
        use tokio_tungstenite::tungstenite::error::{Error, UrlError};
        assert_eq!(last_error::for_dial_error(&Error::Url(UrlError::NoHostName)), "relay_bad_url");
        let not_ws = tokio_tungstenite::tungstenite::http::Response::builder()
            .status(404)
            .body(None)
            .unwrap();
        assert_eq!(last_error::for_dial_error(&Error::Http(not_ws)), "relay_bad_url");
        let io = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        assert_eq!(last_error::for_dial_error(&Error::Io(io)), "relay_unreachable");
    }

    #[test]
    fn device_conflict_is_camel_case_json() {
        assert_eq!(
            serde_json::to_value(DeviceConflict::device_id()).unwrap(),
            serde_json::json!({ "kind": "device_id" })
        );
        assert_eq!(
            serde_json::to_value(DeviceConflict::alias("Mac mini")).unwrap(),
            serde_json::json!({ "kind": "alias", "alias": "Mac mini" })
        );
    }

    #[test]
    fn register_host_carries_this_launch_instance_id() {
        let a = protocol::Envelope::register_host_listed("dev", "tok", None);
        let b = protocol::Envelope::register_host_listed("dev", "tok", Some(("air".into(), "1".into())));
        let id = a.instance_id.clone().expect("instance id sent");
        assert_eq!(uuid::Uuid::parse_str(&id).map(|_| ()), Ok(()));
        assert_eq!(b.instance_id.as_deref(), Some(id.as_str()), "stable for the launch");
        assert_eq!(id, instance_id());
    }

    #[test]
    fn hex_roundtrips() {
        let bytes = [0u8, 1, 15, 16, 255, 128, 64];
        let s = pairing_hex(&bytes);
        assert_eq!(s, "00010f10ff8040");
        assert_eq!(pairing_unhex(&s).unwrap(), bytes);
        assert_eq!(pairing_unhex("00FF").unwrap(), vec![0x00, 0xff]);
        assert!(pairing_unhex("xyz").is_none());
        assert!(pairing_unhex("abc").is_none(), "odd length rejected");
    }
}
