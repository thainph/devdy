//! Remote Control (C1) — Host agent (U3).
//!
//! Backend that lets a Controller device drive a running Devdy session over a
//! self-hosted Cloud Relay, end-to-end encrypted (SRS-RC-C1). This module owns:
//!   - the outbound WSS client to the relay ([`agent`]),
//!   - the per-session bound model + link/OTP/session-secret generation ([`session`]),
//!   - the in-process event tap the run drain publishes to ([`bus`]),
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
pub mod forwarder;
pub mod handler;
pub mod meta;
pub mod protocol;
pub mod session;

pub use bus::{RemoteBus, RemoteRunEvent};
pub use meta::{apply_run_meta, RunMetaStore};

use crate::db::Db;
use crate::runs::{BrokerApprovals, RunRegistry};
use session::BoundSession;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tauri::AppHandle;
use tokio::sync::Mutex as TokioMutex;

/// Persisted-config settings keys (DATA-001). `relay_url` + `enabled` live in
/// the `settings` KV; the `device_id` is generated once and also kept there.
pub const KEY_RELAY_URL: &str = "remote_relay_url";
pub const KEY_ENABLED: &str = "remote_enabled";
pub const KEY_DEVICE_ID: &str = "remote_device_id";

/// Live control handle for the running agent task.
struct AgentControl {
    stop: tokio::sync::watch::Sender<bool>,
    handle: tokio::task::JoinHandle<()>,
}

/// App-wide Remote Control state (Tauri managed).
#[derive(Clone)]
pub struct RemoteState {
    pub bus: RemoteBus,
    /// The single bound remote session (one run/room at a time). `None` when no
    /// remote control is active.
    pub bound: Arc<TokioMutex<Option<BoundSession>>>,
    pub connected: Arc<AtomicBool>,
    /// Rooms queued for revoke; drained by the agent → `revoke{room_id}`.
    pub revoke_queue: Arc<TokioMutex<Vec<String>>>,
    pub revoke_signal: Arc<tokio::sync::Notify>,
    /// Signalled when a new BoundSession is set so a running agent announces the
    /// persistent rendezvous to the relay without waiting for a reconnect.
    pub announce_signal: Arc<tokio::sync::Notify>,
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
            revoke_queue: Arc::new(TokioMutex::new(Vec::new())),
            revoke_signal: Arc::new(tokio::sync::Notify::new()),
            announce_signal: Arc::new(tokio::sync::Notify::new()),
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
            return Err("relay URL is not configured".to_string());
        }
        let auth_token = crate::secrets::get_remote_auth_token()
            .ok_or_else(|| "relay auth token is not set (enable requires a token)".to_string())?;
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
            revoke_queue: self.revoke_queue.clone(),
            revoke_signal: self.revoke_signal.clone(),
            announce_signal: self.announce_signal.clone(),
        };
        let config = agent::AgentConfig {
            relay_url,
            device_id,
        };
        let handle = tokio::spawn(agent::run_agent(config, deps, auth_token, stop_rx));
        *self.agent.lock().await = Some(AgentControl {
            stop: stop_tx,
            handle,
        });
        Ok(())
    }

    /// Stop the agent task if running (signals its reconnect loop to exit).
    pub async fn stop(&self) {
        if let Some(ctrl) = self.agent.lock().await.take() {
            let _ = ctrl.stop.send(true);
            ctrl.handle.abort();
        }
        self.connected
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
