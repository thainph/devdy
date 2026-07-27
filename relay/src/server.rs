//! WebSocket relay server (SRS §2.1, Topology B).
//!
//! Binds `127.0.0.1:<port>` (TLS is terminated by the reverse proxy, CON-01).
//! Each connection gets a read task (this module) and a write task fed by an
//! mpsc channel. Shared room state lives behind a single async `Mutex`.
//!
//! The relay reads only `t` and `room_id`; `cipher` is forwarded opaque and is
//! never logged (SEC-006 / NFR-005).

use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message;

use crate::config::Config;
use crate::protocol::{Envelope, ErrorCode, FrameType};
use crate::ratelimit::RateLimiter;
use crate::room::{ConnId, JoinOutcome, ResumeOutcome, RoomRegistry};

/// Shared server state.
pub struct Shared {
    pub config: Config,
    pub rooms: Mutex<RoomRegistry>,
    next_conn: AtomicU64,
}

impl Shared {
    fn new(config: Config) -> Self {
        Shared {
            config,
            rooms: Mutex::new(RoomRegistry::default()),
            next_conn: AtomicU64::new(1),
        }
    }

    fn alloc_conn(&self) -> ConnId {
        self.next_conn.fetch_add(1, Ordering::Relaxed)
    }
}

/// Handle returned by [`serve`], exposing the actually-bound address (useful
/// when binding to port 0 in tests).
pub struct RelayHandle {
    pub local_addr: SocketAddr,
    pub shared: Arc<Shared>,
}

/// Bind and start accepting connections. Runs until the returned future is
/// dropped/aborted. Returns once the listener is bound so tests can connect.
pub async fn serve(config: Config) -> std::io::Result<(RelayHandle, impl std::future::Future<Output = ()>)> {
    let listener = TcpListener::bind(&config.bind).await?;
    let local_addr = listener.local_addr()?;
    let shared = Arc::new(Shared::new(config));

    tracing::info!(event = "relay_bound", addr = %local_addr, "relay listening (localhost, Topology B)");

    let handle = RelayHandle {
        local_addr,
        shared: shared.clone(),
    };

    let accept_shared = shared.clone();
    let accept_loop = async move {
        // Background sweeper for TTL expiry and idle rooms (SRS §6.2).
        let sweep_shared = accept_shared.clone();
        tokio::spawn(async move { sweep_loop(sweep_shared).await });

        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    let s = accept_shared.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_connection(s, stream, peer).await {
                            tracing::debug!(event = "conn_ended", error = %e);
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!(event = "accept_error", error = %e);
                }
            }
        }
    };

    Ok((handle, accept_loop))
}

/// Background loop that expires pending pair codes and idle rooms.
async fn sweep_loop(shared: Arc<Shared>) {
    let idle = shared.config.room_idle_timeout;
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    loop {
        ticker.tick().await;
        let now = Instant::now();
        let closed = {
            let mut rooms = shared.rooms.lock().await;
            rooms.sweep(now, idle)
        };
        for (room_id, survivors) in closed {
            tracing::info!(event = "room_swept", room_id = %room_id);
            for sink in survivors {
                let _ = sink.send(Envelope::peer_left(room_id.clone()));
            }
        }
    }
}

/// Per-connection role, learned from the first control frame.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Unknown,
    Host,
    Controller,
}

async fn handle_connection(
    shared: Arc<Shared>,
    stream: TcpStream,
    peer: SocketAddr,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ws = tokio_tungstenite::accept_async(stream).await?;
    let conn_id = shared.alloc_conn();
    tracing::info!(event = "conn_open", conn = conn_id, peer = %peer);

    let (mut ws_sink, mut ws_stream) = ws.split();

    // Write task: drains the outbound channel to the socket.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Envelope>();
    let writer = tokio::spawn(async move {
        while let Some(env) = out_rx.recv().await {
            match serde_json::to_string(&env) {
                Ok(text) => {
                    if ws_sink.send(Message::Text(text)).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!(event = "serialize_error", error = %e);
                }
            }
        }
        let _ = ws_sink.close().await;
    });

    let mut role = Role::Unknown;
    let mut limiter = RateLimiter::new(
        shared.config.rate_limit_max,
        shared.config.rate_limit_window,
        Instant::now(),
    );

    // Read loop.
    while let Some(msg) = ws_stream.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!(event = "ws_read_error", conn = conn_id, error = %e);
                break;
            }
        };

        let text = match msg {
            Message::Text(t) => t,
            Message::Binary(_) => {
                // Protocol is JSON text; ignore binary but do not drop the conn.
                continue;
            }
            Message::Ping(_) | Message::Pong(_) => continue,
            Message::Close(_) => break,
            Message::Frame(_) => continue,
        };

        // Rate-limit BEFORE parsing to blunt flooding (SEC-007).
        if !limiter.allow(Instant::now()) {
            tracing::warn!(event = "rate_limited", conn = conn_id);
            let _ = out_tx.send(Envelope::error(ErrorCode::RateLimited, "too many frames"));
            continue;
        }

        let env: Envelope = match serde_json::from_str(&text) {
            Ok(e) => e,
            Err(_) => {
                // Never log the raw text (may contain cipher). Only metadata.
                tracing::debug!(event = "bad_frame", conn = conn_id);
                let _ = out_tx.send(Envelope::error(ErrorCode::BadRequest, "malformed frame"));
                continue;
            }
        };

        let close = dispatch(&shared, conn_id, &mut role, &out_tx, env).await;
        if close {
            break;
        }
    }

    // Cleanup on disconnect: close the room (or keep it alive for a Controller
    // resume) and notify the survivor.
    let survivor = {
        let mut rooms = shared.rooms.lock().await;
        rooms.on_disconnect(conn_id, Instant::now(), shared.config.reconnect_window)
    };
    if let Some((room_id, sinks)) = survivor {
        tracing::info!(event = "peer_left", conn = conn_id, room_id = %room_id);
        for sink in sinks {
            let _ = sink.send(Envelope::peer_left(room_id.clone()));
        }
    }

    drop(out_tx);
    let _ = writer.await;
    tracing::info!(event = "conn_close", conn = conn_id);
    Ok(())
}

/// Handle one decoded envelope. Returns `true` if the connection should close
/// (e.g. auth failure per AC-02).
async fn dispatch(
    shared: &Arc<Shared>,
    conn_id: ConnId,
    role: &mut Role,
    out_tx: &mpsc::UnboundedSender<Envelope>,
    env: Envelope,
) -> bool {
    match env.t {
        FrameType::RegisterHost => {
            // AC-01 / AC-02: authenticate the Host.
            let token_ok = env
                .auth_token
                .as_deref()
                .map(|t| constant_time_eq(t, &shared.config.host_token))
                .unwrap_or(false);
            if !token_ok {
                tracing::warn!(event = "register_denied", conn = conn_id);
                let _ = out_tx.send(Envelope::error(ErrorCode::AuthFailed, "invalid auth token"));
                // Close connection; Host must not retry indefinitely (BR-010).
                return true;
            }
            *role = Role::Host;
            tracing::info!(
                event = "host_registered",
                conn = conn_id,
                device_id = env.device_id.as_deref().unwrap_or("")
            );
            let _ = out_tx.send(Envelope::control_ack(String::new()));
            false
        }

        FrameType::CreatePair => {
            if *role != Role::Host {
                let _ = out_tx.send(Envelope::error(ErrorCode::Forbidden, "not a registered host"));
                return false;
            }
            let pair_code = match env.pair_code.as_deref() {
                Some(p) if !p.is_empty() => p,
                _ => {
                    let _ = out_tx.send(Envelope::error(ErrorCode::BadRequest, "missing pair_code"));
                    return false;
                }
            };
            let now = Instant::now();
            let room_id = new_room_id();
            // A persistent rendezvous (durable reconnect) must survive while the
            // trusted device is offline, so it is not bound by PAIR_TTL. It still
            // dies with the Host connection (on_disconnect closes the Host's rooms).
            let persistent = env.persistent.unwrap_or(false);
            let expires_at = if persistent {
                now + Duration::from_secs(3650 * 24 * 3600)
            } else {
                now + shared.config.pair_ttl
            };
            let hash = hash_code(pair_code);
            {
                let mut rooms = shared.rooms.lock().await;
                rooms.create_pending(room_id.clone(), hash, conn_id, out_tx.clone(), now, expires_at);
            }
            tracing::info!(event = "pair_created", conn = conn_id, room_id = %room_id);
            // Ack with the assigned room_id so the Host can track it for revoke.
            let _ = out_tx.send(Envelope::room_ready(room_id));
            false
        }

        FrameType::Join => {
            let pair_code = match env.pair_code.as_deref() {
                Some(p) if !p.is_empty() => p,
                _ => {
                    let _ = out_tx.send(Envelope::error(ErrorCode::BadRequest, "missing pair_code"));
                    return false;
                }
            };
            let now = Instant::now();
            let hash = hash_code(pair_code);
            // Mint the first rotating resume token; the Controller presents it
            // (hashed) in a later `resume` to re-attach after a drop.
            let resume_token = new_resume_token();
            let resume_hash = hash_code(&resume_token);
            let outcome = {
                let mut rooms = shared.rooms.lock().await;
                rooms.join(hash, conn_id, out_tx.clone(), resume_hash, now)
            };
            match outcome {
                JoinOutcome::Joined {
                    room_id,
                    host_out,
                    controller_out,
                } => {
                    *role = Role::Controller;
                    tracing::info!(event = "join_ok", conn = conn_id, room_id = %room_id);
                    // Notify both peers (SRS §5.3 room_ready → both). Only the
                    // Controller receives the resume token.
                    let _ = host_out.send(Envelope::room_ready(room_id.clone()));
                    let _ = controller_out
                        .send(Envelope::room_ready_with_token(room_id, resume_token));
                    false
                }
                JoinOutcome::Invalid => {
                    // AC-05: expired/used/unknown pair code → refuse, no room.
                    tracing::info!(event = "join_invalid", conn = conn_id);
                    let _ =
                        out_tx.send(Envelope::error(ErrorCode::PairInvalid, "pair code invalid or expired"));
                    false
                }
                JoinOutcome::Full => {
                    // AC-16: second controller refused (MAX_CONTROLLER=1).
                    tracing::info!(event = "join_room_full", conn = conn_id);
                    let _ = out_tx.send(Envelope::error(ErrorCode::RoomFull, "room already has a controller"));
                    false
                }
            }
        }

        FrameType::Resume => {
            let token = match env.resume_token.as_deref() {
                Some(t) if !t.is_empty() => t,
                _ => {
                    let _ = out_tx.send(Envelope::error(ErrorCode::BadRequest, "missing resume_token"));
                    return false;
                }
            };
            let now = Instant::now();
            let old_hash = hash_code(token);
            // Rotate: mint a fresh token to hand back on success.
            let new_token = new_resume_token();
            let new_hash = hash_code(&new_token);
            let outcome = {
                let mut rooms = shared.rooms.lock().await;
                rooms.resume(old_hash, new_hash, conn_id, out_tx.clone(), now)
            };
            match outcome {
                ResumeOutcome::Resumed {
                    room_id,
                    host_out,
                    controller_out,
                } => {
                    *role = Role::Controller;
                    tracing::info!(event = "resume_ok", conn = conn_id, room_id = %room_id);
                    // Host re-offers the handshake on receiving room_ready; the
                    // Controller learns its rotated token.
                    let _ = host_out.send(Envelope::room_ready(room_id.clone()));
                    let _ = controller_out
                        .send(Envelope::room_ready_with_token(room_id, new_token));
                }
                ResumeOutcome::Invalid => {
                    tracing::info!(event = "resume_invalid", conn = conn_id);
                    let _ = out_tx.send(Envelope::error(
                        ErrorCode::ResumeInvalid,
                        "resume token invalid or window elapsed",
                    ));
                }
            }
            false
        }

        FrameType::HandshakeDone => {
            let now = Instant::now();
            let result = {
                let mut rooms = shared.rooms.lock().await;
                rooms.mark_handshake_done(&env.room_id, conn_id, now)
            };
            if let Some((room_id, became_active)) = result {
                if became_active {
                    tracing::info!(event = "room_active", conn = conn_id, room_id = %room_id);
                }
            } else {
                let _ = out_tx.send(Envelope::error(ErrorCode::RoomNotFound, "no room for connection"));
            }
            false
        }

        FrameType::Revoke => {
            // FR-010 / AC-13: only the Host may revoke; close room immediately.
            let now_host = {
                let rooms = shared.rooms.lock().await;
                rooms.is_host(conn_id)
            };
            if !now_host {
                let _ = out_tx.send(Envelope::error(ErrorCode::Forbidden, "only host can revoke"));
                return false;
            }
            let room_id = if env.room_id.is_empty() {
                let rooms = shared.rooms.lock().await;
                rooms.room_of(conn_id)
            } else {
                Some(env.room_id.clone())
            };
            if let Some(room_id) = room_id {
                let survivors = {
                    let mut rooms = shared.rooms.lock().await;
                    rooms.close_room(&room_id, Some(conn_id))
                };
                tracing::info!(event = "room_revoked", conn = conn_id, room_id = %room_id);
                for sink in survivors {
                    let _ = sink.send(Envelope::peer_left(room_id.clone()));
                }
            } else {
                let _ = out_tx.send(Envelope::error(ErrorCode::RoomNotFound, "no room to revoke"));
            }
            false
        }

        // Data frames: forwarded opaque by room_id. Data frames require an
        // ACTIVE room; handshake `control` frames are also allowed while the
        // room is HANDSHAKING so the E2E offer/answer can complete.
        FrameType::Stream | FrameType::PermissionRequest | FrameType::Cmd | FrameType::Control => {
            let now = Instant::now();
            let room_id = env.room_id.clone();
            let allow_handshaking = matches!(env.t, FrameType::Control);
            let targets = {
                let mut rooms = shared.rooms.lock().await;
                rooms.route(&room_id, conn_id, now, allow_handshaking)
            };
            match targets {
                Some(t) => {
                    // seq-based loss/dup detection is metadata-only (SRS §5.1);
                    // logged as a hint, never blocking.
                    if let Some(seq) = env.seq {
                        tracing::trace!(event = "route", room_id = %room_id, seq = seq, sinks = t.sinks.len());
                    }
                    for sink in t.sinks {
                        // Forward verbatim; cipher stays opaque and unlogged.
                        let _ = sink.send(env.clone());
                    }
                }
                None => {
                    tracing::debug!(event = "route_dropped", conn = conn_id, room_id = %room_id);
                    let _ = out_tx.send(Envelope::error(ErrorCode::RoomNotFound, "room not active"));
                }
            }
            false
        }

        // Application-level heartbeat: refresh the room's idle clock (so a live
        // but idle session is not swept) and echo back so the relay→peer leg stays
        // warm too. Cheap and unauthenticated-by-payload — carries no cipher.
        FrameType::Keepalive => {
            if !env.room_id.is_empty() {
                let now = Instant::now();
                let mut rooms = shared.rooms.lock().await;
                rooms.touch(&env.room_id, conn_id, now);
            }
            let _ = out_tx.send(Envelope::keepalive(env.room_id));
            false
        }

        // Frames the relay only ever emits; a client sending them is a no-op.
        FrameType::RoomReady | FrameType::PeerLeft | FrameType::Error => {
            tracing::debug!(event = "ignored_inbound_control", conn = conn_id);
            false
        }
    }
}

/// Generate a routing room id (`rm_<uuid-v4-simple>`).
fn new_room_id() -> String {
    format!("rm_{}", uuid::Uuid::new_v4().simple())
}

/// Generate a rotating resume token. High-entropy (128-bit UUID) so it cannot be
/// guessed within the reconnect window; only its hash is retained by the relay.
fn new_resume_token() -> String {
    format!("rt_{}", uuid::Uuid::new_v4().simple())
}

/// Hash a raw pair code so only the hash is retained (DATA-004).
///
/// This is a routing/index key, not a security control; the security of the
/// pair code rests on its entropy + TTL + one-time use at the application layer.
fn hash_code(code: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    code.hash(&mut h);
    h.finish()
}

/// Length-then-content comparison that avoids early-exit on the first differing
/// byte, reducing timing signal on the shared host token (SEC-001).
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
