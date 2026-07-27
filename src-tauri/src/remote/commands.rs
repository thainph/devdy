//! Tauri commands the Host UI (U5) invokes to drive Remote Control.
//!
//! Config (relay_url/enabled/device_id) lives in `settings`; secrets
//! (auth_token/session) live in the Keychain (`crate::secrets`). No command ever
//! returns a secret value except the session LINK, which is meant to be shown
//! once as a QR/copy-link and carries the high-entropy `link_secret`
//! out-of-band (never sent to the relay or persisted server-side).

use crate::db::Db;
use crate::remote::audit::{self, AuditEntry};
use crate::remote::protocol::LinkPayload;
use crate::remote::{self, session, RemoteState};
use crate::runs::{BrokerApprovals, RunRegistry};
use serde::Serialize;
use tauri::{AppHandle, State};

/// Snapshot of Remote Control status for the UI badge.
#[derive(Debug, Serialize)]
pub struct RemoteStatus {
    /// Whether the feature is enabled in settings.
    pub enabled: bool,
    /// Whether the agent task is running.
    pub running: bool,
    /// Whether a relay connection is currently up + authenticated.
    pub connected: bool,
    /// The configured relay URL ("" when unset).
    pub relay_url: String,
    /// Whether an auth token is present in the keyring (never the value).
    pub has_auth_token: bool,
    /// Whether an owner master password is set (never the value). When true a
    /// controller may authenticate with it instead of the per-join OTP.
    pub has_master_password: bool,
    /// The Host device id (generated once), when known.
    pub device_id: Option<String>,
    /// The single bound run, when a remote session is active.
    pub bound_run_id: Option<String>,
    /// Whether the bound session has authenticated a controller.
    pub session_authenticated: bool,
    /// Sliding idle expiry (Unix secs) of the bound session, when known.
    pub session_idle_expires_at: Option<u64>,
}

/// Persist relay config (DATA-001). `auth_token`, when provided (non-empty), is
/// stored in the Keychain (SEC-001) and never in SQLite. An empty/omitted token
/// leaves the stored token unchanged.
#[tauri::command]
pub async fn remote_set_config(
    db: State<'_, Db>,
    relay_url: String,
    auth_token: Option<String>,
) -> Result<(), String> {
    remote::set_setting(db.inner(), remote::KEY_RELAY_URL, relay_url.trim()).await?;
    if let Some(token) = auth_token {
        if !token.trim().is_empty() {
            crate::secrets::set_remote_auth_token(token.trim()).map_err(|e| e.to_string())?;
        }
    }
    let _ = remote::ensure_device_id(db.inner()).await;
    Ok(())
}

/// Enable Remote Control: persist the flag and start the agent (FR-001).
#[tauri::command]
pub async fn remote_enable(
    app: AppHandle,
    db: State<'_, Db>,
    registry: State<'_, RunRegistry>,
    broker_approvals: State<'_, BrokerApprovals>,
    state: State<'_, RemoteState>,
) -> Result<(), String> {
    remote::set_setting(db.inner(), remote::KEY_ENABLED, "true").await?;
    state
        .start(&app, db.inner(), registry.inner(), broker_approvals.inner())
        .await
}

/// Disable Remote Control: stop the agent and persist the flag.
#[tauri::command]
pub async fn remote_disable(
    db: State<'_, Db>,
    state: State<'_, RemoteState>,
) -> Result<(), String> {
    remote::set_setting(db.inner(), remote::KEY_ENABLED, "false").await?;
    state.stop().await;
    Ok(())
}

/// Start (or supersede) a per-session remote link for `run_id`. Mints a fresh
/// [`session::BoundSession`] and returns the [`LinkPayload`] the desktop encodes
/// as a QR / copy-link. The `link_secret` is ONLY in the return value — it is
/// never persisted server-side and never sent to the relay.
///
/// Superseding an existing session tears the old one down first: its relay room
/// is queued for revoke and its persisted session credential deleted, so exactly
/// ONE session is ever bound (single-session invariant).
#[tauri::command]
pub async fn remote_create_session_link(
    db: State<'_, Db>,
    state: State<'_, RemoteState>,
    run_id: String,
) -> Result<LinkPayload, String> {
    let relay_url = remote::get_setting(db.inner(), remote::KEY_RELAY_URL)
        .await
        .ok_or_else(|| "relay URL is not configured".to_string())?;
    if !state.is_running().await {
        return Err("enable Remote Control before starting a session".to_string());
    }
    if run_id.trim().is_empty() {
        return Err("run_id is required".to_string());
    }

    // Supersede any existing bound session: queue its room for revoke (best
    // effort — the room_id isn't tracked separately, so revoke via rendezvous)
    // and delete the persisted credential BEFORE binding the new one.
    let old_rendezvous = {
        let bound = state.bound.lock().await;
        bound.as_ref().map(|b| b.rendezvous_code.clone())
    };
    if let Some(rv) = old_rendezvous {
        state.revoke_queue.lock().await.push(rv);
        state.revoke_signal.notify_one();
    }
    let _ = crate::secrets::delete_remote_session();

    let bound = session::generate(&run_id)?;
    let link = LinkPayload {
        relay_url,
        host_fingerprint: bound.host_fingerprint.clone(),
        rendezvous_code: bound.rendezvous_code.clone(),
        run_id: run_id.clone(),
        link_secret: bound.link_secret.clone(),
    };
    *state.bound.lock().await = Some(bound);
    // Wake the agent so it announces the persistent rendezvous now.
    state.announce_signal.notify_one();
    state.connected.store(false, std::sync::atomic::Ordering::SeqCst);

    audit::audit(
        db.inner(),
        None,
        None,
        Some(&run_id),
        "create_session_link",
        audit::RESULT_ACCEPTED,
        None,
    )
    .await;
    Ok(link)
}

/// End the current remote session: close the relay room, delete the persisted
/// session credential, and clear the bound session.
#[tauri::command]
pub async fn remote_end_session(
    db: State<'_, Db>,
    state: State<'_, RemoteState>,
) -> Result<(), String> {
    let (rendezvous, run_id) = {
        let mut bound = state.bound.lock().await;
        match bound.take() {
            Some(b) => (Some(b.rendezvous_code), Some(b.run_id)),
            None => (None, None),
        }
    };
    if let Some(rv) = rendezvous {
        state.revoke_queue.lock().await.push(rv);
        state.revoke_signal.notify_one();
    }
    let _ = crate::secrets::delete_remote_session();
    state
        .connected
        .store(false, std::sync::atomic::Ordering::SeqCst);
    audit::audit(
        db.inner(),
        None,
        None,
        run_id.as_deref(),
        "end_session",
        audit::RESULT_ACCEPTED,
        None,
    )
    .await;
    Ok(())
}

/// The current pairing OTP for a run, surfaced on demand (so the desktop can
/// re-show the code without waiting for a controller to (re)join).
#[derive(Debug, Serialize)]
pub struct OtpReveal {
    /// The 6-digit code the controller must enter.
    pub otp: String,
    /// Absolute expiry (Unix seconds) of this code.
    pub otp_expires_at: u64,
    /// Remaining OTP attempts before the code is burned.
    pub attempts_left: u8,
}

/// Reveal (minting if none is live) the pairing OTP for the bound run, so the
/// Host UI can show it on demand instead of only when a controller joins. A live
/// code is reused so it matches what a controller will actually need. Returns
/// `None` when no session is bound to `run_id`.
#[tauri::command]
pub async fn remote_reveal_otp(
    state: State<'_, RemoteState>,
    run_id: String,
) -> Result<Option<OtpReveal>, String> {
    use std::time::{Instant, SystemTime, UNIX_EPOCH};
    let mut bound = state.bound.lock().await;
    let Some(b) = bound.as_mut() else {
        return Ok(None);
    };
    if b.run_id != run_id {
        return Ok(None);
    }
    let now = Instant::now();
    b.ensure_otp(now);
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Ok(Some(OtpReveal {
        otp: b.otp.clone(),
        otp_expires_at: now_unix + b.otp_remaining_secs(now),
        attempts_left: b.otp_attempts_left,
    }))
}

/// Set (or clear) the owner's remote master password. An empty string clears it
/// (falls back to the per-join OTP). Stored in the Keychain, never in SQLite.
#[tauri::command]
pub async fn remote_set_master_password(
    db: State<'_, Db>,
    password: String,
) -> Result<(), String> {
    crate::secrets::set_remote_master_password(&password).map_err(|e| e.to_string())?;
    let action = if password.trim().is_empty() {
        "clear_master_password"
    } else {
        "set_master_password"
    };
    audit::audit(db.inner(), None, None, None, action, audit::RESULT_ACCEPTED, None).await;
    Ok(())
}

/// Read the audit log (FR-011). `limit` defaults to 200 when None/<=0.
#[tauri::command]
pub async fn remote_get_audit(
    db: State<'_, Db>,
    limit: Option<i64>,
) -> Result<Vec<AuditEntry>, String> {
    let limit = limit.filter(|n| *n > 0).unwrap_or(200);
    audit::list_audit(db.inner(), limit).await
}

/// Current Remote Control status for the UI badge.
#[tauri::command]
pub async fn remote_status(
    db: State<'_, Db>,
    state: State<'_, RemoteState>,
) -> Result<RemoteStatus, String> {
    let enabled = remote::get_setting(db.inner(), remote::KEY_ENABLED)
        .await
        .map(|v| v == "true")
        .unwrap_or(false);
    let relay_url = remote::get_setting(db.inner(), remote::KEY_RELAY_URL)
        .await
        .unwrap_or_default();
    let device_id = remote::get_setting(db.inner(), remote::KEY_DEVICE_ID).await;
    let (bound_run_id, session_authenticated) = {
        let bound = state.bound.lock().await;
        match bound.as_ref() {
            Some(b) => (Some(b.run_id.clone()), b.authenticated),
            None => (None, false),
        }
    };
    let session_idle_expires_at =
        crate::secrets::get_remote_session().map(|s| s.idle_expires_at);
    Ok(RemoteStatus {
        enabled,
        running: state.is_running().await,
        connected: state.connected.load(std::sync::atomic::Ordering::SeqCst),
        relay_url,
        has_auth_token: crate::secrets::has_remote_auth_token(),
        has_master_password: crate::secrets::has_remote_master_password(),
        device_id,
        bound_run_id,
        session_authenticated,
        session_idle_expires_at,
    })
}
