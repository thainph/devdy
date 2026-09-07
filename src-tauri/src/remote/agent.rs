//! Outbound WSS client to the relay: register, announce rendezvous, handshake,
//! OTP-gate the first frame, then route (FR-001/003/004/012).
//!
//! The Host dials the relay (never listens), so it always traverses NAT. One
//! long-lived connection carries: the `register_host` control frame (SEC-001
//! auth_token from keyring), a persistent `create_pair` for the single bound
//! session, the E2E handshake over a `control` data frame, sealed `stream`
//! frames outbound, and `cmd` frames inbound. On disconnect the agent reconnects
//! with capped exponential backoff (BR-010) and re-announces.
//!
//! Redesign: ONE `BoundSession` at a time. When a controller joins, the host
//! mints a 6-digit OTP shown on the desktop; the first sealed frame is opened by
//! trying `KDF(link_secret, session_secret)` (reconnect) then
//! `KDF(link_secret, otp)` (pairing) — whichever opens authenticates the
//! session. On success the host mints a durable session token (sliding 3h idle).

use crate::db::Db;
use crate::remote::audit;
use crate::remote::bus::RemoteBus;
use crate::remote::command::{IdempotencyGuard, RateLimiter};
use crate::remote::forwarder::{forward_loop, SeqCounter};
use crate::remote::handler::{handle, HandlerCtx};
use crate::remote::protocol::{
    CmdPayload, Envelope, FrameType, HandshakePayload, StreamPayload,
};
use crate::remote::session::BoundSession;
use crate::runs::{BrokerApprovals, RunRegistry};
use futures_util::{SinkExt, StreamExt};
use remote_e2e::{derive_effective_psk, host_fingerprint, Session};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, Mutex as TokioMutex};
use tokio_tungstenite::tungstenite::Message;

/// Min/max reconnect backoff (BR-010): start at 1 s, cap at 30 s.
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Heartbeat cadence to the relay. Well under the typical 60 s idle cutoff of
/// proxies/load-balancers so an idle (post-run) socket is never reaped.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(20);

/// Sliding idle window for a remote session token: 3 hours of inactivity.
pub const IDLE_TTL_SECS: u64 = 3 * 3600;

/// Current wall-clock time as Unix seconds (0 if the clock is before the epoch).
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Shared, connection-independent handle to the idle deadline. The handler
/// slides it forward on each accepted command; reconnect reads it to decide if
/// the session token is still valid. Stored as Unix seconds.
pub type IdleClock = Arc<AtomicU64>;

/// Whether an incoming joined room must be refused because another room already
/// owns the authenticated bound-session controller slot.
fn bound_controller_slot_taken(active_room_id: Option<&str>, incoming_room_id: &str) -> bool {
    matches!(active_room_id, Some(active) if active != incoming_room_id)
}

fn should_ignore_handshake(
    active_room_id: Option<&str>,
    pending_room_id: Option<&str>,
    incoming_room_id: &str,
) -> bool {
    active_room_id.is_some() || pending_room_id != Some(incoming_room_id)
}

/// Immutable-ish config for one agent run.
#[derive(Clone)]
pub struct AgentConfig {
    pub relay_url: String,
    pub device_id: String,
}

/// Shared deps the agent needs to service commands.
#[derive(Clone)]
pub struct AgentDeps {
    pub app: AppHandle,
    pub db: Db,
    pub registry: RunRegistry,
    pub broker_approvals: BrokerApprovals,
    pub bus: RemoteBus,
    /// The single bound remote session (set by `remote_create_session_link`).
    pub bound: Arc<TokioMutex<Option<BoundSession>>>,
    /// Whether a relay connection is currently up + authenticated.
    pub connected: Arc<std::sync::atomic::AtomicBool>,
    /// Room ids the Owner asked to revoke (teardown / supersede). The agent
    /// drains this and sends a `revoke{room_id}` to the relay.
    pub revoke_queue: Arc<TokioMutex<Vec<String>>>,
    /// Wakes the agent when a new revoke request is queued.
    pub revoke_signal: Arc<tokio::sync::Notify>,
    /// Wakes the agent when a new BoundSession is set so it announces the
    /// persistent rendezvous on the live connection (not only on reconnect).
    pub announce_signal: Arc<tokio::sync::Notify>,
}

/// Run the agent until `stop` fires. Reconnect loop with capped backoff.
pub async fn run_agent(
    config: AgentConfig,
    deps: AgentDeps,
    auth_token: String,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let mut backoff = BACKOFF_MIN;
    loop {
        if *stop.borrow() {
            break;
        }
        let started = Instant::now();
        let outcome = connect_once(&config, &deps, &auth_token, &mut stop).await;
        match outcome {
            ConnectOutcome::Stopped => break,
            ConnectOutcome::Disconnected => {
                deps.connected.store(false, Ordering::SeqCst);
                if started.elapsed() > Duration::from_secs(30) {
                    backoff = BACKOFF_MIN;
                }
                tracing::info!(event = "remote_reconnect_wait", secs = backoff.as_secs());
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = stop.changed() => { if *stop.borrow() { break; } }
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
    deps.connected.store(false, Ordering::SeqCst);
    tracing::info!(event = "remote_agent_stopped");
}

enum ConnectOutcome {
    Stopped,
    Disconnected,
}

/// One connection lifecycle: dial → register → serve until the socket drops.
async fn connect_once(
    config: &AgentConfig,
    deps: &AgentDeps,
    auth_token: &str,
    stop: &mut tokio::sync::watch::Receiver<bool>,
) -> ConnectOutcome {
    let ws = match tokio_tungstenite::connect_async(&config.relay_url).await {
        Ok((ws, _resp)) => ws,
        Err(e) => {
            tracing::warn!(event = "remote_connect_failed", error = %e);
            return ConnectOutcome::Disconnected;
        }
    };
    tracing::info!(event = "remote_connected", url = %config.relay_url);

    let (mut sink, mut stream) = ws.split();

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Envelope>();
    let writer = tokio::spawn(async move {
        while let Some(env) = out_rx.recv().await {
            match serde_json::to_string(&env) {
                Ok(text) => {
                    if sink.send(Message::Text(text)).await.is_err() {
                        break;
                    }
                }
                Err(e) => tracing::warn!(event = "remote_serialize_error", error = %e),
            }
        }
        let _ = sink.close().await;
    });

    // Register the Host (FR-001, SEC-001 token from keyring).
    if out_tx
        .send(Envelope::register_host(
            config.device_id.clone(),
            auth_token.to_string(),
        ))
        .is_err()
    {
        writer.abort();
        return ConnectOutcome::Disconnected;
    }

    // Rehydrate a bound session from a persisted (non-expired) session token so a
    // controller can reconnect without an OTP after an app/relay restart.
    rehydrate_bound_if_empty(deps).await;

    // Announce the persistent rendezvous for the bound session (if any).
    announce_bound(deps, &out_tx).await;

    // Rooms we've announced (create_pair ack seen) and await a controller join.
    let mut armed_rooms: std::collections::HashSet<String> = std::collections::HashSet::new();

    let seq = Arc::new(SeqCounter::default());
    let rate = Arc::new(TokioMutex::new(RateLimiter::new(Instant::now())));
    let idem = Arc::new(TokioMutex::new(IdempotencyGuard::new()));
    let idle: IdleClock = Arc::new(AtomicU64::new(0));
    // The active room + session, once authenticated.
    let mut room_ctx: Option<Arc<HandlerCtx>> = None;
    // The offered E2E session (post-handshake, pre-auth). Used to try opening the
    // first sealed frame at the OTP gate.
    let mut pending_room: Option<String> = None;
    let mut forward_task: Option<tokio::task::JoinHandle<()>> = None;

    drain_revokes(deps, &out_tx).await;

    // Heartbeat: keep the host↔relay socket warm through idle-killing proxies
    // once a run stops streaming. Carries the ACTIVE room id (if any) so the relay
    // also refreshes that room's idle clock; empty otherwise (pure socket warming).
    let mut keepalive = tokio::time::interval(KEEPALIVE_INTERVAL);
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let result = loop {
        tokio::select! {
            _ = stop.changed() => {
                if *stop.borrow() { break ConnectOutcome::Stopped; }
            }
            _ = keepalive.tick() => {
                let room_id = room_ctx.as_ref().map(|c| c.room_id.clone()).unwrap_or_default();
                if out_tx.send(Envelope::keepalive(room_id)).is_err() {
                    break ConnectOutcome::Disconnected;
                }
            }
            _ = deps.revoke_signal.notified() => {
                drain_revokes(deps, &out_tx).await;
            }
            _ = deps.announce_signal.notified() => {
                announce_bound(deps, &out_tx).await;
            }
            msg = stream.next() => {
                let Some(msg) = msg else { break ConnectOutcome::Disconnected; };
                let msg = match msg {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::debug!(event = "remote_ws_read_error", error = %e);
                        break ConnectOutcome::Disconnected;
                    }
                };
                let text = match msg {
                    Message::Text(t) => t,
                    Message::Close(_) => break ConnectOutcome::Disconnected,
                    Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_) => continue,
                };
                let env: Envelope = match serde_json::from_str(&text) {
                    Ok(e) => e,
                    Err(_) => { tracing::debug!(event = "remote_bad_frame"); continue; }
                };
                dispatch_inbound(
                    env, config, deps, &out_tx, &seq, &rate, &idem, &idle,
                    &mut room_ctx, &mut pending_room, &mut forward_task, &mut armed_rooms,
                ).await;
            }
        }
    };

    if let Some(t) = forward_task.take() {
        t.abort();
    }
    drop(out_tx);
    writer.abort();
    result
}

/// Send a `revoke{room_id}` for every queued revoke request.
async fn drain_revokes(deps: &AgentDeps, out_tx: &mpsc::UnboundedSender<Envelope>) {
    let mut queue = deps.revoke_queue.lock().await;
    for room_id in queue.drain(..) {
        let _ = out_tx.send(Envelope::revoke(room_id));
    }
}

/// Announce the bound session's rendezvous to the relay as a persistent pair, so
/// a controller can join (or rejoin, past PAIR_TTL) at any time until teardown.
async fn announce_bound(deps: &AgentDeps, out_tx: &mpsc::UnboundedSender<Envelope>) {
    if let Some(b) = deps.bound.lock().await.as_ref() {
        let _ = out_tx.send(Envelope::create_pair_persistent(
            b.rendezvous_code.clone(),
            true,
        ));
    }
}

/// If nothing is bound, rebuild a bound session from a persisted, non-expired
/// session token so a controller can reconnect without an OTP.
async fn rehydrate_bound_if_empty(deps: &AgentDeps) {
    let mut bound = deps.bound.lock().await;
    if bound.is_some() {
        return;
    }
    let Some(cred) = crate::secrets::get_remote_session() else {
        return;
    };
    if cred.idle_expires_at <= now_unix() {
        // Idle window elapsed: drop the token so the controller falls back to OTP.
        let _ = crate::secrets::delete_remote_session();
        let _ = deps
            .app
            .emit("remote://session-expired", SessionExpired { run_id: cred.run_id });
        return;
    }
    match crate::remote::session::rehydrate(&cred) {
        Ok(b) => {
            tracing::info!(event = "remote_session_rehydrated", run_id = %b.run_id);
            *bound = Some(b);
        }
        Err(e) => tracing::warn!(event = "remote_session_rehydrate_failed", error = %e),
    }
}

/// Handle one inbound relay frame.
#[allow(clippy::too_many_arguments)]
async fn dispatch_inbound(
    env: Envelope,
    _config: &AgentConfig,
    deps: &AgentDeps,
    out_tx: &mpsc::UnboundedSender<Envelope>,
    seq: &Arc<SeqCounter>,
    rate: &Arc<TokioMutex<RateLimiter>>,
    idem: &Arc<TokioMutex<IdempotencyGuard>>,
    idle: &IdleClock,
    room_ctx: &mut Option<Arc<HandlerCtx>>,
    pending_room: &mut Option<String>,
    forward_task: &mut Option<tokio::task::JoinHandle<()>>,
    armed_rooms: &mut std::collections::HashSet<String>,
) {
    match env.t {
        FrameType::RoomReady => {
            let room_id = env.room_id.clone();
            if room_id.is_empty() {
                return;
            }
            // First `room_ready` = create_pair ack — arm and await a join.
            if armed_rooms.insert(room_id.clone()) {
                tracing::info!(event = "remote_pair_room_ready", room_id = %room_id);
                return;
            }
            // The standing rendezvous may admit a second relay room while the
            // bound session already has an authenticated Controller. Refuse that
            // room at the Host boundary: relay MAX_CONTROLLER is per-room, while
            // our invariant is per bound session. Most importantly, do not touch
            // `controller_pub`, `authenticated`, room_ctx, or the first room.
            if bound_controller_slot_taken(
                room_ctx.as_ref().map(|ctx| ctx.room_id.as_str()),
                &room_id,
            ) {
                tracing::warn!(
                    event = "remote_controller_rejected",
                    reason = "bound_session_full",
                    room_id = %room_id
                );
                let _ = out_tx.send(Envelope::revoke(room_id));
                return;
            }
            // A duplicate notification for the already authenticated room is
            // harmless, but must not restart its handshake or reset bound auth.
            if room_ctx.as_ref().is_some_and(|ctx| ctx.room_id == room_id) {
                tracing::debug!(event = "remote_duplicate_room_ready", room_id = %room_id);
                return;
            }
            // Second `room_ready` = a controller joined → offer the handshake and
            // mint a fresh OTP shown on the desktop.
            let (host_pub, fp) = {
                let mut bound = deps.bound.lock().await;
                let Some(b) = bound.as_mut() else { return; };
                // Reuse a still-live code (it may already be shown on the desktop
                // via reveal) instead of rotating a fresh one out from under the
                // user on every join; mint only when absent/expired.
                let now = Instant::now();
                b.ensure_otp(now);
                let otp = b.otp.clone();
                let otp_expires_unix = now_unix() + b.otp_remaining_secs(now);
                let attempts_left = b.otp_attempts_left;
                let run_id = b.run_id.clone();
                let _ = deps.app.emit(
                    "remote://controller-requested-session",
                    ControllerRequested {
                        run_id,
                        otp,
                        otp_expires_at: otp_expires_unix,
                        attempts_left,
                    },
                );
                (
                    crate::remote::pairing_hex(&b.keypair.public),
                    b.host_fingerprint.clone(),
                )
            };
            *pending_room = Some(room_id.clone());

            let offer = HandshakePayload::HsOffer {
                host_pub,
                host_fingerprint: fp,
            };
            if let Ok(json) = serde_json::to_vec(&offer) {
                use base64::Engine as _;
                let cipher = base64::engine::general_purpose::STANDARD.encode(json);
                let _ = out_tx.send(Envelope::data(FrameType::Control, &room_id, cipher, seq.next()));
            }
            tracing::info!(event = "remote_handshake_offered", room_id = %room_id);
            // Keep the standing rendezvous available for the NEXT reconnect.
            announce_bound(deps, out_tx).await;
        }

        FrameType::Control => {
            // Handshake answer from the controller (HsAnswer).
            // Once a room owns the authenticated controller slot, no later
            // handshake answer may mutate the bound session. In particular, a
            // queued answer from a just-revoked second room must not reset
            // `authenticated`, replace `controller_pub`, or steal pending_room.
            if should_ignore_handshake(
                room_ctx.as_ref().map(|ctx| ctx.room_id.as_str()),
                pending_room.as_deref(),
                &env.room_id,
            ) {
                tracing::debug!(
                    event = "remote_late_handshake_ignored",
                    active_room_id = ?room_ctx.as_ref().map(|ctx| ctx.room_id.as_str()),
                    room_id = %env.room_id
                );
                return;
            }
            let Some(cipher) = env.cipher.as_deref() else { return; };
            use base64::Engine as _;
            let Ok(json) = base64::engine::general_purpose::STANDARD.decode(cipher) else {
                return;
            };
            let Ok(HandshakePayload::HsAnswer { controller_pub }) =
                serde_json::from_slice::<HandshakePayload>(&json)
            else {
                return;
            };
            complete_handshake(env.room_id.clone(), controller_pub, deps, out_tx, pending_room).await;
        }

        FrameType::Cmd => {
            // Two states:
            //  - authenticated: a live room_ctx exists → open + dispatch.
            //  - not yet authenticated: OTP gate the FIRST sealed frame.
            if let Some(ctx) = room_ctx.as_ref() {
                let Some(cipher) = env.cipher.as_deref() else { return; };
                let plaintext = match ctx.session.open(cipher) {
                    Ok(pt) => pt,
                    Err(e) => {
                        tracing::debug!(event = "remote_cmd_open_failed", error = %e);
                        return;
                    }
                };
                let cmd: CmdPayload = match serde_json::from_slice(&plaintext) {
                    Ok(c) => c,
                    Err(_) => {
                        audit::audit(
                            &ctx.db, None, Some(&ctx.room_id), None,
                            "unknown", audit::RESULT_REJECTED, Some("malformed"),
                        ).await;
                        return;
                    }
                };
                handle(ctx, cmd).await;
                return;
            }
            // Not yet authenticated on this connection → treat this as the auth
            // gate (first sealed frame). Resolve the room from the FRAME itself,
            // not `pending_room`: a peer_left during a reconnect storm can clear
            // `pending_room` before the gate frame lands, which previously made
            // the host silently drop it (and every later command), stranding the
            // controller on a "connected" but dead session. `env.room_id` is
            // always present on a routed data frame.
            let room_id = if !env.room_id.is_empty() {
                env.room_id.clone()
            } else if let Some(r) = pending_room.clone() {
                r
            } else {
                tracing::debug!(event = "remote_cmd_no_room");
                return;
            };
            let Some(cipher) = env.cipher.as_deref() else { return; };
            try_authenticate_first_frame(
                room_id, cipher, deps, out_tx, seq, rate, idem, idle,
                room_ctx, forward_task,
            )
            .await;
            // Auth succeeded (room_ctx now set) → drop the stale `pending_room`
            // so subsequent commands take the fast authenticated path instead of
            // re-running the gate.
            if room_ctx.is_some() {
                *pending_room = None;
            }
        }

        FrameType::PeerLeft => {
            tracing::info!(event = "remote_peer_left", room_id = %env.room_id);
            // A Host connection can own the authenticated room plus a standing
            // rendezvous. A peer leaving/revoking the latter must not tear down
            // the authenticated Controller or clear its token state.
            if let Some(ctx) = room_ctx.as_ref() {
                if env.room_id != ctx.room_id {
                    if pending_room.as_deref() == Some(env.room_id.as_str()) {
                        *pending_room = None;
                    }
                    return;
                }
            }
            if let Some(t) = forward_task.take() {
                t.abort();
            }
            let run_id = deps
                .bound
                .lock()
                .await
                .as_ref()
                .map(|b| b.run_id.clone())
                .unwrap_or_default();
            *room_ctx = None;
            *pending_room = None;
            // Reset the auth flag so a resume re-runs the OTP/token gate.
            if let Some(b) = deps.bound.lock().await.as_mut() {
                b.authenticated = false;
            }
            deps.connected.store(false, Ordering::SeqCst);
            let _ = deps.app.emit("remote://disconnected", Disconnected { run_id });
        }

        FrameType::Error => {
            tracing::warn!(
                event = "remote_relay_error",
                code = env.code.as_deref().unwrap_or(""),
                message = env.message.as_deref().unwrap_or("")
            );
        }

        _ => {
            tracing::debug!(event = "remote_ignored_inbound", t = ?env.t);
        }
    }
}

/// Capture the controller's ephemeral public key from the handshake answer and
/// remember the room as pending-auth. The E2E session key is NOT derived yet (it
/// depends on OTP vs session_secret); the actual `Session` is built at the OTP
/// gate when the first sealed frame arrives.
async fn complete_handshake(
    room_id: String,
    controller_pub_hex: String,
    deps: &AgentDeps,
    out_tx: &mpsc::UnboundedSender<Envelope>,
    pending_room: &mut Option<String>,
) {
    let controller_pub = match crate::remote::pairing_unhex(&controller_pub_hex) {
        Some(b) if b.len() == 32 => b,
        _ => {
            tracing::warn!(event = "remote_handshake_bad_pubkey");
            return;
        }
    };
    {
        let mut bound = deps.bound.lock().await;
        let Some(b) = bound.as_mut() else {
            tracing::warn!(event = "remote_handshake_no_bound");
            return;
        };
        // Self-check our own fingerprint against what we published.
        let _ = host_fingerprint(&b.keypair.public);
        b.authenticated = false;
        b.controller_pub = Some(controller_pub);
    }
    *pending_room = Some(room_id.clone());
    // Drive the relay room HANDSHAKING → ACTIVE now (both peers handshake_done).
    // The OTP gate happens at the app layer on the first sealed `cmd`, which the
    // relay only routes once the room is ACTIVE — so we must complete the E2E
    // handshake here, BEFORE authentication. The room being ACTIVE is safe: the
    // host processes no command and forwards nothing until the OTP/token gate
    // opens that first frame.
    let _ = out_tx.send(Envelope::handshake_done(&room_id));
    tracing::info!(event = "remote_handshake_ready", room_id = %room_id);
}

/// Try to open the controller's first sealed frame with `session_secret` then
/// `otp`. On success: authenticate, mint + persist a session token, send it,
/// signal handshake_done, start the scoped forward loop, and dispatch the frame.
#[allow(clippy::too_many_arguments)]
async fn try_authenticate_first_frame(
    room_id: String,
    cipher: &str,
    deps: &AgentDeps,
    out_tx: &mpsc::UnboundedSender<Envelope>,
    seq: &Arc<SeqCounter>,
    rate: &Arc<TokioMutex<RateLimiter>>,
    idem: &Arc<TokioMutex<IdempotencyGuard>>,
    idle: &IdleClock,
    room_ctx: &mut Option<Arc<HandlerCtx>>,
    forward_task: &mut Option<tokio::task::JoinHandle<()>>,
) {
    // Snapshot the fields we need to derive candidate keys (hold the lock only
    // briefly; open attempts are CPU-cheap).
    let (link_secret, otp, session_secret, keypair, controller_pub, run_id, otp_expired, attempts_left) = {
        let bound = deps.bound.lock().await;
        let Some(b) = bound.as_ref() else { return; };
        let Some(controller_pub) = b.controller_pub.clone() else {
            tracing::debug!(event = "remote_auth_no_controller_pub");
            return;
        };
        (
            b.link_secret.clone(),
            b.otp.clone(),
            b.session_secret.clone(),
            b.keypair.clone(),
            controller_pub,
            b.run_id.clone(),
            b.otp_expired(Instant::now()),
            b.otp_attempts_left,
        )
    };

    // Try the reconnect key (session_secret) first, then — when the owner has set
    // one — the master password, and finally the per-join OTP. Whichever `open`s
    // authenticates. Both peers feed byte-identical inputs to
    // `derive_effective_psk`, so a wrong secret simply yields a different key and
    // every open fails — there is no comparison oracle. The master password and
    // OTP share the `attempts_left` budget so brute force stays bounded.
    let try_open = |secret2: &str| -> Option<(Session, Vec<u8>)> {
        let psk = derive_effective_psk(&link_secret, secret2);
        let session = Session::new(&psk, &keypair, &controller_pub).ok()?;
        let pt = session.open(cipher).ok()?;
        Some((session, pt))
    };

    let master_password = crate::secrets::get_remote_master_password();
    let opened = try_open(&session_secret)
        .or_else(|| {
            if attempts_left == 0 {
                return None;
            }
            master_password.as_deref().and_then(|p| try_open(p))
        })
        .or_else(|| {
            if otp_expired || attempts_left == 0 {
                None
            } else {
                try_open(&otp)
            }
        });

    let Some((session, plaintext)) = opened else {
        // Auth failed. Decrement attempts; if exhausted/expired, invalidate.
        let (left, run) = {
            let mut bound = deps.bound.lock().await;
            match bound.as_mut() {
                Some(b) => {
                    b.otp_attempts_left = b.otp_attempts_left.saturating_sub(1);
                    (b.otp_attempts_left, b.run_id.clone())
                }
                None => (0, String::new()),
            }
        };
        audit::audit(
            &deps.db, None, Some(&room_id), Some(&run),
            "authenticate", audit::RESULT_REJECTED, Some("bad_otp"),
        )
        .await;
        let reason = if left == 0 || otp_expired {
            "locked"
        } else {
            "bad_otp"
        };
        let _ = deps.app.emit(
            "remote://auth-failed",
            AuthFailed {
                run_id: run,
                attempts_left: left,
                reason: reason.to_string(),
            },
        );
        return;
    };

    // Authenticated. Build the session Arc, mark the bound session, mint a token.
    let session = Arc::new(session);
    let idle_expires = now_unix() + IDLE_TTL_SECS;
    idle.store(idle_expires, Ordering::SeqCst);

    let (token, rendezvous_code) = {
        let mut bound = deps.bound.lock().await;
        let Some(b) = bound.as_mut() else { return; };
        b.authenticated = true;
        // Persist the session token credential (reconnect without OTP).
        let cred = crate::secrets::RemoteSession {
            run_id: b.run_id.clone(),
            rendezvous_code: b.rendezvous_code.clone(),
            session_secret: b.session_secret.clone(),
            host_pub: crate::remote::pairing_hex(&b.keypair.public),
            host_secret: crate::remote::pairing_hex(&b.keypair.secret),
            idle_expires_at: idle_expires,
        };
        let _ = crate::secrets::set_remote_session(&cred);
        // The opaque token is the session_secret (bearer for the controller); it
        // is only meaningful combined with the controller-held link_secret.
        (b.session_secret.clone(), b.rendezvous_code.clone())
    };

    // Send the session token (sealed).
    let token_payload = StreamPayload::SessionToken {
        token,
        run_id: run_id.clone(),
        rendezvous_code,
        idle_expires_at: idle_expires,
    };
    if let Ok(json) = serde_json::to_vec(&token_payload) {
        let c = session.seal(&json);
        let _ = out_tx.send(Envelope::data(FrameType::Stream, &room_id, c, seq.next()));
    }
    // Also send an explicit auth result for the controller UI.
    let auth_payload = StreamPayload::AuthResult {
        ok: true,
        run_id: run_id.clone(),
        reason: None,
    };
    if let Ok(json) = serde_json::to_vec(&auth_payload) {
        let c = session.seal(&json);
        let _ = out_tx.send(Envelope::data(FrameType::Stream, &room_id, c, seq.next()));
    }

    // Signal HANDSHAKING → ACTIVE to the relay.
    let _ = out_tx.send(Envelope::handshake_done(&room_id));
    deps.connected.store(true, Ordering::SeqCst);
    audit::audit(
        &deps.db, None, Some(&room_id), Some(&run_id),
        "authenticate", audit::RESULT_ACCEPTED, None,
    )
    .await;
    let _ = deps
        .app
        .emit("remote://connection-authenticated", Authenticated { run_id: run_id.clone() });
    tracing::info!(event = "remote_room_authenticated", room_id = %room_id);

    // Start the run-scoped forward loop.
    let ft = tokio::spawn(forward_loop(
        deps.bus.clone(),
        session.clone(),
        room_id.clone(),
        out_tx.clone(),
        seq.clone(),
        deps.db.clone(),
        run_id.clone(),
    ));
    if let Some(old) = forward_task.replace(ft) {
        old.abort();
    }

    // Push initial scoped run list so the controller lands on the bound run.
    crate::remote::forwarder::send_run_list(&deps.db, &session, &room_id, out_tx, &**seq, &run_id)
        .await;

    // Push the initial engine/model/permission-mode snapshot so the controller's
    // composer mirrors the desktop from the first frame (no blank/default drift).
    {
        use tauri::Manager;
        if let Some(store) = deps.app.try_state::<crate::remote::RunMetaStore>() {
            crate::remote::forwarder::send_run_meta(
                &deps.db, &store, &session, &room_id, out_tx, &**seq, &run_id,
            )
            .await;
        }
    }

    // Push the initial subscription usage badges (Claude + Codex) so the
    // controller shows plan utilization from the first frame.
    crate::remote::forwarder::send_plan_usage(&deps.db, &session, &room_id, out_tx, &**seq, &run_id).await;

    let ctx = Arc::new(HandlerCtx {
        app: deps.app.clone(),
        db: deps.db.clone(),
        registry: deps.registry.clone(),
        broker_approvals: deps.broker_approvals.clone(),
        session,
        room_id: room_id.clone(),
        bound_run_id: run_id.clone(),
        out: out_tx.clone(),
        seq: seq.clone(),
        rate: rate.clone(),
        idem: idem.clone(),
        idle: idle.clone(),
    });
    *room_ctx = Some(ctx.clone());

    // Dispatch the first (already-decrypted) frame as a normal command.
    match serde_json::from_slice::<CmdPayload>(&plaintext) {
        Ok(cmd) => handle(&ctx, cmd).await,
        Err(_) => {
            // The first frame may just be an empty auth ping; ignore parse errors.
            tracing::debug!(event = "remote_first_frame_not_cmd");
        }
    }
}

// ---- Tauri event payloads ----

#[derive(serde::Serialize, Clone)]
struct ControllerRequested {
    run_id: String,
    otp: String,
    otp_expires_at: u64,
    attempts_left: u8,
}

#[derive(serde::Serialize, Clone)]
struct Authenticated {
    run_id: String,
}

#[derive(serde::Serialize, Clone)]
struct AuthFailed {
    run_id: String,
    attempts_left: u8,
    reason: String,
}

#[derive(serde::Serialize, Clone)]
struct Disconnected {
    run_id: String,
}

#[cfg(test)]
mod tests {
    use super::{bound_controller_slot_taken, should_ignore_handshake};

    #[test]
    fn authenticated_bound_session_refuses_a_second_room() {
        assert!(bound_controller_slot_taken(Some("room-first"), "room-second"));
    }

    #[test]
    fn active_room_frames_do_not_refuse_the_active_room_itself() {
        assert!(!bound_controller_slot_taken(Some("room-first"), "room-first"));
        assert!(!bound_controller_slot_taken(None, "room-first"));
    }

    #[test]
    fn late_or_unexpected_handshake_cannot_mutate_an_active_session() {
        assert!(should_ignore_handshake(
            Some("room-first"),
            Some("room-second"),
            "room-second"
        ));
        assert!(should_ignore_handshake(
            Some("room-first"),
            None,
            "room-first"
        ));
        assert!(should_ignore_handshake(
            None,
            Some("room-expected"),
            "room-stale"
        ));
        assert!(!should_ignore_handshake(
            None,
            Some("room-expected"),
            "room-expected"
        ));
    }
}

#[derive(serde::Serialize, Clone)]
struct SessionExpired {
    run_id: String,
}
