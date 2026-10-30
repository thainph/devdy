//! WebSocket relay server (SRS §2.1, Topology B).
//!
//! Binds `127.0.0.1:<port>` (TLS is terminated by the reverse proxy, CON-01).
//! Each connection gets a read task (this module) and a write task fed by an
//! mpsc channel. Shared room state lives behind a single async `Mutex`.
//!
//! Liveness is per connection and is the only timeout in the relay: the write
//! task pings every `ping_interval`, and a connection that delivers no bytes
//! for `peer_liveness_timeout` is closed. Closing a connection is the one path
//! that tears down its listen/room, so a dead peer is always cleaned up.
//!
//! The relay reads only `t` and `room_id`; `cipher` is forwarded opaque and is
//! never logged (SEC-006 / NFR-005).

use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

use crate::config::Config;
use crate::protocol::{Envelope, ErrorCode, FrameType};
use crate::ratelimit::RateLimiter;
use crate::room::{ConnId, JoinOutcome, Notice, OutFrame, Outbound, RoomRegistry};
use crate::sync::{Limits, SyncHub, SyncSession};

/// Shared server state.
pub struct Shared {
    pub config: Config,
    pub rooms: Mutex<RoomRegistry>,
    pub sync: SyncHub,
    next_conn: AtomicU64,
}

impl Shared {
    fn new(config: Config) -> std::io::Result<Self> {
        let limits = Limits {
            max_group_bytes: config.sync_max_group_bytes,
            max_blob_bytes: config.sync_max_blob_bytes,
        };
        let sync = SyncHub::open(config.sync_db.as_deref(), limits)
            .map_err(|e| std::io::Error::other(format!("sync db: {e}")))?;
        Ok(Shared {
            config,
            rooms: Mutex::new(RoomRegistry::default()),
            sync,
            next_conn: AtomicU64::new(1),
        })
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
    let shared = Arc::new(Shared::new(config)?);

    tracing::info!(event = "relay_bound", addr = %local_addr, "relay listening (localhost, Topology B)");
    tracing::info!(event = "sync_store", enabled = shared.sync.enabled());

    let handle = RelayHandle {
        local_addr,
        shared: shared.clone(),
    };

    let accept_shared = shared.clone();
    let accept_loop = async move {
        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    // OS-level backstop against half-open sockets; the app-level
                    // liveness check in `handle_connection` is the primary one.
                    apply_tcp_keepalive(&stream);
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

/// Per-connection role, learned from the first control frame.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Unknown,
    Host,
    Controller,
    /// Welcomed by `sync_hello`: only sync/blob frames and keepalives.
    Sync,
}

/// A TCP stream that records when it last received any byte. Liveness is
/// judged on raw bytes, not whole messages, so a large upload on a slow link
/// (an image turn from a phone on 4G) never looks like a dead peer.
struct ActivityStream {
    inner: TcpStream,
    born: Instant,
    /// Milliseconds since `born` of the last successful non-empty read.
    last_read_ms: Arc<AtomicU64>,
}

impl tokio::io::AsyncRead for ActivityStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let res = Pin::new(&mut self.inner).poll_read(cx, buf);
        if matches!(res, Poll::Ready(Ok(()))) && buf.filled().len() > before {
            let ms = self.born.elapsed().as_millis() as u64;
            self.last_read_ms.store(ms, Ordering::Relaxed);
        }
        res
    }
}

impl tokio::io::AsyncWrite for ActivityStream {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Deliver room-close notices: `peer_left`, or `error{superseded}` for a
/// Controller that a newer join replaced. A superseded connection is then
/// closed, so even a client that does not know `superseded` stops using it.
///
/// Callers hold the registry lock while delivering (sends never block), so
/// every peer sees notices and `room_ready`s in the order the registry changed.
fn deliver(notices: Vec<Notice>) {
    for n in notices {
        if n.superseded {
            let mut e = Envelope::error(ErrorCode::Superseded, "replaced by a newer connection");
            e.room_id = n.room_id;
            let _ = n.out.send(e);
            n.out.close();
        } else {
            let _ = n.out.send(Envelope::peer_left(n.room_id));
        }
    }
}

async fn handle_connection(
    shared: Arc<Shared>,
    stream: TcpStream,
    peer: SocketAddr,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Cap the frame/message size the library will buffer for us. Without this
    // tokio-tungstenite accepts up to its 64 MiB default per message, which any
    // unauthenticated connection could use to make the relay allocate.
    let max_message_bytes = shared.config.effective_max_message_bytes();
    let ws_config = WebSocketConfig {
        max_message_size: Some(max_message_bytes),
        max_frame_size: Some(max_message_bytes),
        ..Default::default()
    };
    let born = Instant::now();
    let last_read_ms = Arc::new(AtomicU64::new(0));
    let stream = ActivityStream {
        inner: stream,
        born,
        last_read_ms: last_read_ms.clone(),
    };
    // A client that never finishes the WS upgrade must not pin a task forever.
    let ws = tokio::time::timeout(
        shared.config.peer_liveness_timeout,
        tokio_tungstenite::accept_async_with_config(stream, Some(ws_config)),
    )
    .await
    .map_err(|_| "websocket upgrade timed out")??;
    let conn_id = shared.alloc_conn();
    tracing::info!(event = "conn_open", conn = conn_id, peer = %peer);

    let (mut ws_sink, mut ws_stream) = ws.split();

    // Write task: drains the outbound channel to the socket and pings on a
    // steady cadence. Browsers answer pings natively, so even a backgrounded
    // phone tab proves it is alive without running any JS.
    let (out_tx, mut out_rx) = crate::room::outbound_channel();
    let ping_interval = shared.config.ping_interval;
    let writer = tokio::spawn(async move {
        let mut ping = tokio::time::interval(ping_interval);
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let msg = tokio::select! {
                env = out_rx.recv() => match env {
                    Some(OutFrame::Env(env)) => match serde_json::to_string(&env) {
                        Ok(text) => Message::Text(text),
                        Err(e) => {
                            tracing::warn!(event = "serialize_error", error = %e);
                            continue;
                        }
                    },
                    Some(OutFrame::Text(text)) => Message::Text(text),
                    None => break,
                },
                _ = ping.tick() => Message::Ping(Vec::new()),
            };
            if ws_sink.send(msg).await.is_err() {
                break;
            }
        }
        let _ = ws_sink.close().await;
    });

    let mut role = Role::Unknown;
    let mut sync_session: Option<SyncSession> = None;
    let mut limiter = RateLimiter::new(
        shared.config.rate_limit_max,
        shared.config.rate_limit_window,
        Instant::now(),
    );
    let liveness = shared.config.peer_liveness_timeout;
    let mut liveness_check = tokio::time::interval(Duration::from_secs(1));
    liveness_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // Read loop.
    loop {
        let msg = tokio::select! {
            msg = ws_stream.next() => match msg {
                Some(Ok(m)) => m,
                Some(Err(e)) => {
                    tracing::debug!(event = "ws_read_error", conn = conn_id, error = %e);
                    break;
                }
                None => break,
            },
            _ = liveness_check.tick() => {
                let last = Duration::from_millis(last_read_ms.load(Ordering::Relaxed));
                if born.elapsed().saturating_sub(last) >= liveness {
                    tracing::info!(event = "conn_liveness_timeout", conn = conn_id);
                    break;
                }
                continue;
            }
            _ = out_tx.killed() => {
                tracing::warn!(event = "conn_too_slow", conn = conn_id);
                break;
            }
        };

        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            // Protocol is JSON text; pings/pongs only matter for liveness,
            // which the byte-level clock already recorded.
            Message::Binary(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
        };

        let env: Envelope = match serde_json::from_str(&text) {
            Ok(e) => e,
            Err(_) => {
                // Never log the raw text (may contain cipher). Only metadata.
                tracing::debug!(event = "bad_frame", conn = conn_id);
                let _ = out_tx.send(Envelope::error(ErrorCode::BadRequest, "malformed frame"));
                continue;
            }
        };

        // Rate-limit control frames to blunt join/register flooding (SEC-007).
        // Data and keepalives are exempt: a streaming Host legitimately sends
        // far more than any control budget, and dropping its frames would
        // corrupt the session.
        if is_rate_limited(env.t) && !limiter.allow(Instant::now()) {
            tracing::warn!(event = "rate_limited", conn = conn_id);
            let _ = out_tx.send(Envelope::error(ErrorCode::RateLimited, "too many frames"));
            continue;
        }

        // Sync connection: sync/blob frames go to the sync store; nothing of
        // Remote Control is reachable from it (and vice versa).
        if role == Role::Sync || crate::sync::is_sync_frame(env.t) {
            let close = match (role, env.t) {
                (Role::Host | Role::Controller, _) => {
                    let _ = out_tx.send(Envelope::error(ErrorCode::Forbidden, "not a sync connection"));
                    false
                }
                (_, t) if crate::sync::is_sync_frame(t) => {
                    shared
                        .sync
                        .handle(conn_id, &mut sync_session, &out_tx, &text, &shared.config.host_token)
                        .await
                }
                (_, FrameType::Keepalive) => {
                    let _ = out_tx.send(Envelope::keepalive(String::new()));
                    false
                }
                _ => {
                    let _ = out_tx.send(Envelope::error(ErrorCode::Forbidden, "only sync frames on a sync connection"));
                    false
                }
            };
            if sync_session.is_some() {
                role = Role::Sync;
            }
            if close {
                break;
            }
            continue;
        }

        if dispatch(&shared, conn_id, &mut role, &out_tx, env).await {
            break;
        }
    }

    shared.sync.on_disconnect(conn_id, sync_session.as_ref());

    // Cleanup: drop the Host's listen / close the room, notify the survivor.
    let notices = shared.rooms.lock().await.on_disconnect(conn_id);
    deliver(notices);

    // Let the writer flush what is queued (e.g. an auth error) and close, but
    // never wait on a socket that stopped draining.
    drop(out_tx);
    let mut writer = writer;
    if tokio::time::timeout(Duration::from_secs(2), &mut writer).await.is_err() {
        writer.abort();
    }
    tracing::info!(event = "conn_close", conn = conn_id);
    Ok(())
}

/// Frames that count against the per-connection rate limit.
fn is_rate_limited(t: FrameType) -> bool {
    !crate::sync::is_sync_data_frame(t)
        && !matches!(
        t,
        FrameType::Stream | FrameType::PermissionRequest | FrameType::Cmd | FrameType::Control | FrameType::Keepalive
    )
}

/// Handle one decoded envelope. Returns `true` if the connection should close
/// (e.g. auth failure per AC-02).
async fn dispatch(
    shared: &Arc<Shared>,
    conn_id: ConnId,
    role: &mut Role,
    out_tx: &Outbound,
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

        // Host listens on its rendezvous code until it disconnects, revokes, or
        // listens on another code.
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
            let code = hash_code(pair_code);
            let slot = env.slot.clone().unwrap_or_default();
            let next_room_id = new_room_id();
            let mut rooms = shared.rooms.lock().await;
            if rooms.len() >= shared.config.max_rooms && !rooms.is_listening(code) {
                drop(rooms);
                tracing::warn!(event = "room_cap_reached", conn = conn_id);
                let _ = out_tx.send(Envelope::error(ErrorCode::BadRequest, "room cap reached"));
                return false;
            }
            deliver(rooms.listen(code, conn_id, out_tx.clone(), next_room_id.clone(), &slot));
            let mut ack = Envelope::room_armed(next_room_id);
            ack.slot = env.slot;
            let _ = out_tx.send(ack);
            drop(rooms);
            tracing::info!(event = "listening", conn = conn_id);
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
            if *role == Role::Host {
                let _ = out_tx.send(Envelope::error(ErrorCode::Forbidden, "a host cannot join"));
                return false;
            }
            let mut rooms = shared.rooms.lock().await;
            match rooms.join(hash_code(pair_code), conn_id, out_tx.clone(), new_room_id()) {
                JoinOutcome::Joined {
                    room_id,
                    host_out,
                    slot,
                    notices,
                } => {
                    *role = Role::Controller;
                    // Still under the lock: a concurrent join can't interleave
                    // its room_ready with ours at the Host.
                    deliver(notices);
                    let mut ready = Envelope::room_ready(room_id.clone());
                    ready.slot = (!slot.is_empty()).then_some(slot);
                    let _ = host_out.send(ready);
                    let _ = out_tx.send(Envelope::room_ready(room_id.clone()));
                    drop(rooms);
                    tracing::info!(event = "join_ok", conn = conn_id, room_id = %room_id);
                }
                JoinOutcome::Invalid => {
                    drop(rooms);
                    // No Host listening (offline / superseded link). The
                    // Controller retries with backoff.
                    tracing::info!(event = "join_invalid", conn = conn_id);
                    let _ = out_tx.send(Envelope::error(ErrorCode::PairInvalid, "host is not listening"));
                }
            }
            false
        }

        // Legacy resume: always refuse so an older Controller falls back to join.
        FrameType::Resume => {
            let _ = out_tx.send(Envelope::error(
                ErrorCode::ResumeInvalid,
                "resume is not supported; join again",
            ));
            false
        }

        // Legacy: rooms route as soon as they exist.
        FrameType::HandshakeDone => false,

        FrameType::Revoke => {
            if *role != Role::Host {
                let _ = out_tx.send(Envelope::error(ErrorCode::Forbidden, "only host can revoke"));
                return false;
            }
            {
                let mut rooms = shared.rooms.lock().await;
                let slot = env.slot.as_deref().unwrap_or_default();
                deliver(rooms.revoke(conn_id, &env.room_id, hash_code(&env.room_id), slot));
            }
            tracing::info!(event = "revoked", conn = conn_id);
            false
        }

        // Data frames: forwarded opaque to the other member of the room.
        FrameType::Stream | FrameType::PermissionRequest | FrameType::Cmd | FrameType::Control => {
            let target = shared.rooms.lock().await.route(&env.room_id, conn_id);
            match target {
                Some(sink) => {
                    // Forward verbatim; cipher stays opaque and unlogged.
                    let _ = sink.send(env);
                }
                None => {
                    tracing::debug!(event = "route_dropped", conn = conn_id, room_id = %env.room_id);
                    let mut e = Envelope::error(ErrorCode::RoomNotFound, "room not found");
                    e.room_id = env.room_id;
                    let _ = out_tx.send(e);
                }
            }
            false
        }

        // Application-level heartbeat: echo so the relay→peer leg stays warm.
        FrameType::Keepalive => {
            let _ = out_tx.send(Envelope::keepalive(env.room_id));
            false
        }

        // Frames the relay only ever emits; a client sending them is a no-op.
        FrameType::RoomReady | FrameType::PeerLeft | FrameType::Error => {
            tracing::debug!(event = "ignored_inbound_control", conn = conn_id);
            false
        }

        // Sync frames are handled before `dispatch` (see the read loop).
        FrameType::SyncHello
        | FrameType::SyncWelcome
        | FrameType::SyncPush
        | FrameType::SyncAck
        | FrameType::SyncPull
        | FrameType::SyncOps
        | FrameType::SyncNotify
        | FrameType::SyncRevoke
        | FrameType::BlobPut
        | FrameType::BlobAck
        | FrameType::BlobGet
        | FrameType::BlobData
        | FrameType::BlobRelease
        | FrameType::BlobReleased => {
            tracing::debug!(event = "ignored_inbound_control", conn = conn_id);
            false
        }
    }
}

/// Enable TCP keepalive probes on an accepted connection so the kernel reaps a
/// half-open socket (peer vanished with no FIN/RST) in ~1 minute instead of its
/// multi-hour default. Best-effort: a failure here only forfeits this OS-level
/// backstop, and `sweep_peer_liveness` still detects the drop at the app layer.
fn apply_tcp_keepalive(stream: &TcpStream) {
    let ka = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(30))
        .with_interval(Duration::from_secs(10));
    let sref = socket2::SockRef::from(stream);
    if let Err(e) = sref.set_tcp_keepalive(&ka) {
        tracing::debug!(event = "tcp_keepalive_failed", error = %e);
    }
}

/// Generate a routing room id (`rm_<uuid-v4-simple>`).
fn new_room_id() -> String {
    format!("rm_{}", uuid::Uuid::new_v4().simple())
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
pub(crate) fn constant_time_eq(a: &str, b: &str) -> bool {
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
