//! WebSocket client for the relay's sync frames (design §9.2).
//!
//! A sync connection is separate from the Remote Control one but dials the same
//! relay URL with the same `RELAY_HOST_TOKEN`, TLS stack and liveness rules as
//! `remote::agent` (connect timeout, 15 s keepalive, 45 s silence watchdog,
//! wake-from-sleep re-dial). One [`Connection`] = one authenticated socket;
//! reconnecting is the engine's job.
//!
//! The relay stores no data (on-demand-sync.md §P1): it authenticates the
//! devices of a group (at most two) and forwards sealed `sync_peer_send` /
//! `sync_peer_msg` frames between them. Requests are strictly sequential (one
//! in flight per connection) and carry an `id` the relay echoes on the reply
//! or error, which is how replies are matched; a reply without an `id` is
//! matched by type. The unsolicited `sync_group_info` / `sync_peer_msg`
//! frames (and the welcome's `creator_device`) are surfaced as [`Event`]s, so
//! a message from another device never waits behind a request in flight.
//! Payloads (`data`) and tokens are never logged.

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_tungstenite::tungstenite::Message;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const LIVENESS_TIMEOUT: Duration = Duration::from_secs(45);
const LIVENESS_CHECK_INTERVAL: Duration = Duration::from_secs(5);
/// Same heuristic as `remote::agent::SLEEP_JUMP`: wall clock ran ahead of the
/// monotonic clock ⇒ the machine slept and the socket is dead.
const SLEEP_JUMP: Duration = Duration::from_secs(20);
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// `sync_peer_send`: the relay acks once the target's socket took the
/// message, which may take up to 60 s with a slow target.
const PEER_SEND_TIMEOUT: Duration = Duration::from_secs(75);

/// Every sync frame, both directions. Only the fields of a given `t` are set.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Frame {
    pub t: String,
    /// Request correlation id, echoed by the relay on the reply / error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create: Option<bool>,
    /// `sync_hello` of a device joining an existing group: the one-time ticket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
    /// `sync_invite`: hex sha256 of the ticket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<u64>,
    /// `sync_peer_send` / `sync_peer_msg`: base64 sealed `PeerMsg`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// `sync_welcome` / `sync_group_info`: the device that may revoke others.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator_device: Option<String>,
    /// `sync_revoke` of the creator by itself: who takes the creator role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub successor: Option<String>,
    /// Keepalive frames share the Remote Control envelope, which carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_id: Option<String>,
    /// `sync_peer_send`: the device the sealed `data` goes to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_device: Option<String>,
    /// `sync_peer_msg`: the device that sent it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_device: Option<String>,
    /// `sync_peers` reply: the group's other devices with a live connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub devices: Option<Vec<String>>,
}

impl Frame {
    pub fn new(t: &str) -> Self {
        Frame {
            t: t.to_string(),
            ..Default::default()
        }
    }
}

// Hand-written so tokens and payloads can never reach a log line.
impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame")
            .field("t", &self.t)
            .field("data", &self.data.as_ref().map(String::len))
            .field("code", &self.code)
            .finish_non_exhaustive()
    }
}

/// Where and as whom to connect.
#[derive(Clone)]
pub struct ConnectParams {
    pub relay_url: String,
    pub auth_token: String,
    pub group_id: String,
    pub device_id: String,
    pub device_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// Dial / socket / timeout problem — retry with backoff.
    Network(String),
    /// The relay refused us (`auth_failed`, `group_not_found`, `device_revoked`,
    /// …): retrying will not help until the configuration changes.
    Rejected(String),
    /// The relay answered a request with an `error` frame.
    Relay(String),
    /// The connection is gone.
    Closed,
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::Network(e) => write!(f, "relay unreachable: {e}"),
            TransportError::Rejected(c) => write!(f, "relay rejected this device: {c}"),
            TransportError::Relay(c) => write!(f, "relay error: {c}"),
            TransportError::Closed => f.write_str("relay connection closed"),
        }
    }
}

/// Hello rejections that mean "stop retrying".
fn is_fatal_code(code: &str) -> bool {
    matches!(
        code,
        "auth_failed"
            | "group_not_found"
            | "group_exists"
            | "ticket_required"
            | "ticket_invalid"
            | "device_revoked"
            | "forbidden"
            | "sync_disabled"
            | "group_full"
    )
}

/// The relay's refusal of a third device in a group.
pub const GROUP_FULL: &str = "group_full";

/// Longest join ticket lifetime the relay accepts.
#[allow(dead_code)] // pairing (join tickets)
pub const MAX_TICKET_TTL_SECS: u64 = 600;

/// Unsolicited things the connection reports to its owner.
#[derive(Clone, PartialEq, Eq)]
pub enum Event {
    /// The group's creator per the relay (on welcome, and when it moves).
    Creator(String),
    /// `sync_peer_msg`: base64 sealed data another device of the group sent.
    PeerMsg { from: String, data: String },
    Closed,
}

// Hand-written so a peer message's data never reaches a log line.
impl std::fmt::Debug for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Event::Creator(c) => f.debug_tuple("Creator").field(c).finish(),
            Event::PeerMsg { from, data } => f
                .debug_struct("PeerMsg")
                .field("from", from)
                .field("len", &data.len())
                .finish(),
            Event::Closed => f.write_str("Closed"),
        }
    }
}

struct Waiter {
    id: u64,
    expect: &'static [&'static str],
    reply: oneshot::Sender<Frame>,
}

impl Waiter {
    /// The echoed `id` decides when present; otherwise the frame type does.
    fn wants(&self, frame: &Frame) -> bool {
        match frame.id.as_ref() {
            Some(id) => id.as_u64() == Some(self.id),
            None => frame.t == "error" || self.expect.contains(&frame.t.as_str()),
        }
    }
}

/// A live, authenticated sync connection. Dropping it closes the socket.
pub struct Connection {
    out: mpsc::UnboundedSender<Frame>,
    waiter: Arc<Mutex<Option<Waiter>>>,
    /// Serializes requests (one in flight).
    request_lock: Mutex<()>,
    next_id: std::sync::atomic::AtomicU64,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Connection {
    /// Dial, send `sync_hello`, wait for `sync_welcome`. Events (peer
    /// messages, creator, closed) go to `events` for the lifetime of the
    /// connection.
    pub async fn connect(
        params: &ConnectParams,
        create: bool,
        events: mpsc::UnboundedSender<Event>,
    ) -> Result<Connection, TransportError> {
        Self::connect_with_ticket(params, create, None, events).await
    }

    /// [`Connection::connect`] for a device not yet registered in an existing
    /// group: `ticket` is the one-time join ticket a member registered with
    /// [`Connection::sync_invite`]. Once welcomed the device is known and later
    /// connections need no ticket.
    pub async fn connect_with_ticket(
        params: &ConnectParams,
        create: bool,
        ticket: Option<&str>,
        events: mpsc::UnboundedSender<Event>,
    ) -> Result<Connection, TransportError> {
        Self::dial(params, create, ticket, events).await
    }

    async fn dial(
        params: &ConnectParams,
        create: bool,
        ticket: Option<&str>,
        events: mpsc::UnboundedSender<Event>,
    ) -> Result<Connection, TransportError> {
        let dial = tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(&params.relay_url));
        let ws = match dial.await {
            Ok(Ok((ws, _))) => ws,
            Ok(Err(e)) => return Err(TransportError::Network(e.to_string())),
            Err(_) => return Err(TransportError::Network("connect timeout".into())),
        };
        let (mut sink, mut stream) = ws.split();

        let mut hello = Frame::new("sync_hello");
        hello.auth_token = Some(params.auth_token.clone());
        hello.group_id = Some(params.group_id.clone());
        hello.device_id = Some(params.device_id.clone());
        hello.device_token = Some(params.device_token.clone());
        if create {
            hello.create = Some(true);
        }
        hello.ticket = ticket.map(str::to_string);
        send_frame(&mut sink, &hello).await?;

        // Wait for the hello verdict, ignoring anything else (keepalive echoes).
        tokio::time::timeout(REQUEST_TIMEOUT, async {
            while let Some(msg) = stream.next().await {
                let Some(frame) = parse(msg.map_err(|e| TransportError::Network(e.to_string()))?) else {
                    continue;
                };
                match frame.t.as_str() {
                    "sync_welcome" => {
                        if let Some(c) = frame.creator_device.filter(|c| !c.is_empty()) {
                            let _ = events.send(Event::Creator(c));
                        }
                        return Ok(());
                    }
                    "error" => {
                        let code = frame.code.unwrap_or_else(|| "error".into());
                        return Err(if is_fatal_code(&code) {
                            TransportError::Rejected(code)
                        } else {
                            TransportError::Relay(code)
                        });
                    }
                    _ => continue,
                }
            }
            Err(TransportError::Closed)
        })
        .await
        .map_err(|_| TransportError::Network("hello timeout".into()))??;

        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Frame>();
        let waiter: Arc<Mutex<Option<Waiter>>> = Arc::new(Mutex::new(None));
        let task_waiter = waiter.clone();
        let task = tokio::spawn(async move {
            let mut keepalive = tokio::time::interval(KEEPALIVE_INTERVAL);
            keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            keepalive.tick().await;
            let mut liveness = tokio::time::interval(LIVENESS_CHECK_INTERVAL);
            liveness.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut last_inbound = Instant::now();
            let mut last_check = (Instant::now(), SystemTime::now());
            loop {
                tokio::select! {
                    out = out_rx.recv() => {
                        let Some(frame) = out else { break };
                        if send_frame(&mut sink, &frame).await.is_err() { break; }
                    }
                    _ = keepalive.tick() => {
                        let mut ka = Frame::new("keepalive");
                        ka.room_id = Some(String::new());
                        if send_frame(&mut sink, &ka).await.is_err() { break; }
                    }
                    _ = liveness.tick() => {
                        if last_inbound.elapsed() >= LIVENESS_TIMEOUT {
                            tracing::warn!(event = "sync_liveness_timeout");
                            break;
                        }
                        let (mono, wall) = (Instant::now(), SystemTime::now());
                        let wall_delta = wall.duration_since(last_check.1).unwrap_or_default();
                        let mono_delta = mono.duration_since(last_check.0);
                        last_check = (mono, wall);
                        if wall_delta > mono_delta + SLEEP_JUMP {
                            tracing::info!(event = "sync_wake_redial");
                            break;
                        }
                    }
                    msg = stream.next() => {
                        let Some(Ok(msg)) = msg else { break };
                        last_inbound = Instant::now();
                        if matches!(msg, Message::Close(_)) { break; }
                        let Some(frame) = parse(msg) else { continue };
                        match frame.t.as_str() {
                            "sync_group_info" => {
                                if let Some(c) = frame.creator_device.filter(|c| !c.is_empty()) {
                                    let _ = events.send(Event::Creator(c));
                                }
                            }
                            "sync_peer_msg" => {
                                if let (Some(from), Some(data)) = (frame.from_device, frame.data) {
                                    let _ = events.send(Event::PeerMsg { from, data });
                                }
                            }
                            "keepalive" => {}
                            t => {
                                let mut slot = task_waiter.lock().await;
                                if slot.as_ref().is_some_and(|w| w.wants(&frame)) {
                                    if let Some(w) = slot.take() { let _ = w.reply.send(frame); }
                                } else {
                                    tracing::debug!(event = "sync_unexpected_frame", t = %t);
                                }
                            }
                        }
                    }
                }
            }
            // Fail the in-flight request now instead of at its timeout.
            task_waiter.lock().await.take();
            let _ = sink.close().await;
            let _ = events.send(Event::Closed);
        });

        Ok(Connection {
            out: out_tx,
            waiter,
            request_lock: Mutex::new(()),
            next_id: std::sync::atomic::AtomicU64::new(1),
            task,
        })
    }

    /// Close the socket now (requests in flight fail right away).
    pub fn close(&self) {
        self.task.abort();
        if let Ok(mut w) = self.waiter.try_lock() {
            w.take();
        }
    }

    async fn request_within(
        &self,
        mut frame: Frame,
        expect: &'static [&'static str],
        timeout: Duration,
    ) -> Result<Frame, TransportError> {
        let _one = self.request_lock.lock().await;
        let id = self.next_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        frame.id = Some(id.into());
        let (tx, rx) = oneshot::channel();
        *self.waiter.lock().await = Some(Waiter { id, expect, reply: tx });
        if self.out.send(frame).is_err() {
            self.waiter.lock().await.take();
            return Err(TransportError::Closed);
        }
        let reply = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(f)) => f,
            Ok(Err(_)) => return Err(TransportError::Closed),
            Err(_) => {
                self.waiter.lock().await.take();
                return Err(TransportError::Network("request timeout".into()));
            }
        };
        if reply.t == "error" {
            let code = reply.code.unwrap_or_else(|| "error".into());
            return Err(if is_fatal_code(&code) {
                TransportError::Rejected(code)
            } else {
                TransportError::Relay(code)
            });
        }
        Ok(reply)
    }

    /// `sync_peers` → the group's other devices with a live connection.
    pub async fn peers(&self) -> Result<Vec<String>, TransportError> {
        let r = self.request_within(Frame::new("sync_peers"), &["sync_peers"], Duration::from_secs(10)).await?;
        Ok(r.devices.unwrap_or_default())
    }

    /// `sync_peer_send{to_device, data}` → `sync_peer_ack` once the relay
    /// queued it for `to`; `Relay("peer_offline")` when `to` has no live
    /// connection, `Relay("too_large")` over the relay's blob cap.
    pub async fn peer_send(&self, to: &str, data_b64: String) -> Result<(), TransportError> {
        let mut f = Frame::new("sync_peer_send");
        f.to_device = Some(to.to_string());
        f.data = Some(data_b64);
        self.request_within(f, &["sync_peer_ack"], PEER_SEND_TIMEOUT).await.map(|_| ())
    }

    /// `sync_invite{ticket_hash, ttl_secs}` → `control{code:"ok"}`: register a
    /// one-time join ticket (`ticket_hash` = lowercase hex sha256 of the
    /// ticket, `ttl_secs` ≤ [`MAX_TICKET_TTL_SECS`]).
    #[allow(dead_code)] // pairing (join tickets)
    pub async fn sync_invite(&self, ticket_hash: &str, ttl_secs: u64) -> Result<(), TransportError> {
        let mut f = Frame::new("sync_invite");
        f.ticket_hash = Some(ticket_hash.to_string());
        f.ttl_secs = Some(ttl_secs);
        let r = self.request_within(f, &["control"], Duration::from_secs(10)).await?;
        match r.code.as_deref() {
            Some("ok") => Ok(()),
            other => Err(TransportError::Relay(other.unwrap_or("unexpected reply").to_string())),
        }
    }

    /// `sync_revoke{device_id}` → `control{code:"ok"}`. A device may revoke
    /// itself; only the group's creator may revoke another (`forbidden`).
    pub async fn revoke(&self, device_id: &str) -> Result<(), TransportError> {
        self.revoke_with(device_id, None).await
    }

    /// [`Connection::revoke`] naming the device that takes the creator role
    /// when the creator revokes itself (the relay falls back to the oldest
    /// active member when `successor` is not one).
    pub async fn revoke_with(&self, device_id: &str, successor: Option<&str>) -> Result<(), TransportError> {
        let mut f = Frame::new("sync_revoke");
        f.device_id = Some(device_id.to_string());
        f.successor = successor.map(str::to_string);
        let r = self.request_within(f, &["control"], Duration::from_secs(10)).await?;
        match r.code.as_deref() {
            Some("ok") => Ok(()),
            other => Err(TransportError::Relay(other.unwrap_or("unexpected reply").to_string())),
        }
    }
}

type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Message,
>;

async fn send_frame(sink: &mut WsSink, frame: &Frame) -> Result<(), TransportError> {
    let text = serde_json::to_string(frame).map_err(|e| TransportError::Network(e.to_string()))?;
    sink.send(Message::Text(text))
        .await
        .map_err(|e| TransportError::Network(e.to_string()))
}

fn parse(msg: Message) -> Option<Frame> {
    match msg {
        Message::Text(t) => serde_json::from_str(&t).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_use_the_contract_field_names() {
        let mut f = Frame::new("sync_peer_send");
        f.to_device = Some("b".into());
        f.data = Some("c2VhbGVk".into());
        assert_eq!(
            serde_json::to_value(&f).unwrap(),
            serde_json::json!({ "t": "sync_peer_send", "to_device": "b", "data": "c2VhbGVk" })
        );
        // A welcome without the old store fields parses.
        let w: Frame = serde_json::from_str(r#"{"t":"sync_welcome","creator_device":"a","room_id":""}"#).unwrap();
        assert_eq!(w.creator_device.as_deref(), Some("a"));
    }

    #[test]
    fn debug_never_prints_secrets() {
        let mut f = Frame::new("sync_hello");
        f.auth_token = Some("relay-secret".into());
        f.device_token = Some("device-secret".into());
        f.data = Some("sealed-data".into());
        let s = format!("{f:?}");
        assert!(!s.contains("secret") && !s.contains("sealed-data"), "{s}");
    }

    fn params(relay: &crate::sync::test_relay::TestRelay, device: &str) -> ConnectParams {
        ConnectParams {
            relay_url: relay.url(),
            auth_token: crate::sync::test_relay::TestRelay::TOKEN.into(),
            group_id: "g".into(),
            device_id: device.into(),
            device_token: format!("tok-{device}"),
        }
    }

    fn sha_hex(s: &str) -> String {
        use sha2::Digest as _;
        hex::encode(sha2::Sha256::digest(s.as_bytes()))
    }

    #[tokio::test]
    async fn joining_an_existing_group_takes_a_one_time_ticket() {
        let relay = crate::sync::test_relay::TestRelay::start().await;
        let (tx, _rx) = mpsc::unbounded_channel();
        let a = Connection::connect(&params(&relay, "a"), true, tx).await.unwrap();
        let join = |device: &'static str, ticket: Option<&'static str>, create: bool| {
            let p = params(&relay, device);
            async move {
                let (tx, _rx) = mpsc::unbounded_channel();
                Connection::connect_with_ticket(&p, create, ticket, tx).await.map(drop).err()
            }
        };
        let rejected = |c: &str| Some(TransportError::Rejected(c.into()));
        assert_eq!(join("b", None, false).await, rejected("ticket_required"));
        assert_eq!(join("b", None, true).await, rejected("group_exists"));
        assert_eq!(join("b", Some("t1"), false).await, rejected("ticket_invalid"));

        a.sync_invite(&sha_hex("t1"), 60).await.unwrap();
        assert_eq!(join("b", Some("t1"), false).await, None);
        assert_eq!(join("c", Some("t1"), false).await, rejected("ticket_invalid"), "one use");
        // Known now: no ticket needed.
        assert_eq!(join("b", None, false).await, None);
        assert_eq!(
            a.sync_invite(&sha_hex("t2"), MAX_TICKET_TTL_SECS + 1).await.err(),
            Some(TransportError::Relay("bad_request".into()))
        );
    }

    /// Connect `device` to group `g` (created by `creator`) with a fresh ticket.
    async fn join(
        relay: &crate::sync::test_relay::TestRelay,
        creator: &Connection,
        device: &str,
    ) -> Result<(Connection, mpsc::UnboundedReceiver<Event>), TransportError> {
        let ticket = format!("ticket-{device}");
        creator.sync_invite(&sha_hex(&ticket), 60).await.unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        let c = Connection::connect_with_ticket(&params(relay, device), false, Some(&ticket), tx).await?;
        Ok((c, rx))
    }

    #[tokio::test]
    async fn a_group_takes_two_devices() {
        let relay = crate::sync::test_relay::TestRelay::start().await;
        let (tx_a, _rx_a) = mpsc::unbounded_channel();
        let a = Connection::connect(&params(&relay, "a"), true, tx_a).await.unwrap();
        let (b, _rx_b) = join(&relay, &a, "b").await.unwrap();
        let full = join(&relay, &a, "c").await.err();
        assert_eq!(full, Some(TransportError::Rejected(GROUP_FULL.into())));
        // Known devices still connect.
        let (tx, _rx) = mpsc::unbounded_channel();
        assert!(Connection::connect(&params(&relay, "b"), false, tx).await.is_ok());
        // Once one leaves, there is room again.
        drop(b);
        a.revoke("b").await.unwrap();
        assert!(join(&relay, &a, "c").await.is_ok());
    }

    #[tokio::test]
    async fn revoke_kicks_the_device_and_blocks_its_hello() {
        let relay = crate::sync::test_relay::TestRelay::start().await;
        let (tx_a, _rx_a) = mpsc::unbounded_channel();
        let a = Connection::connect(&params(&relay, "a"), true, tx_a).await.unwrap();
        let (b, mut rx_b) = join(&relay, &a, "b").await.unwrap();
        // Only the creator revokes others.
        assert_eq!(b.revoke("a").await.err(), Some(TransportError::Rejected("forbidden".into())));

        a.revoke("b").await.unwrap();
        let ev = loop {
            match tokio::time::timeout(Duration::from_secs(5), rx_b.recv()).await.unwrap() {
                Some(Event::Creator(_)) => continue,
                other => break other,
            }
        };
        drop(b);
        assert_eq!(ev, Some(Event::Closed));
        let (tx, _rx) = mpsc::unbounded_channel();
        let again = Connection::connect(&params(&relay, "b"), false, tx).await.err();
        assert_eq!(again, Some(TransportError::Rejected("device_revoked".into())));

        // A wrong device token for a known device is refused too.
        let mut bad = params(&relay, "a");
        bad.device_token = "stolen".into();
        let (tx, _rx) = mpsc::unbounded_channel();
        let err = Connection::connect(&bad, false, tx).await.err();
        assert_eq!(err, Some(TransportError::Rejected("auth_failed".into())));
    }

    async fn recv(rx: &mut mpsc::UnboundedReceiver<Event>) -> Option<Event> {
        tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap()
    }

    #[tokio::test]
    async fn creator_leaving_hands_the_role_to_its_successor() {
        let relay = crate::sync::test_relay::TestRelay::start().await;
        let (tx_a, mut rx_a) = mpsc::unbounded_channel();
        let a = Connection::connect(&params(&relay, "a"), true, tx_a).await.unwrap();
        assert_eq!(recv(&mut rx_a).await, Some(Event::Creator("a".into())), "welcome names the creator");
        let (b, mut rx_b) = join(&relay, &a, "b").await.unwrap();
        assert_eq!(recv(&mut rx_b).await, Some(Event::Creator("a".into())));

        let _ = a.revoke_with("a", Some("b")).await;
        assert_eq!(recv(&mut rx_b).await, Some(Event::Creator("b".into())));
        // The new creator lets a new device in.
        assert!(join(&relay, &b, "c").await.is_ok());
    }

    #[tokio::test]
    async fn store_frames_are_unsupported() {
        let relay = crate::sync::test_relay::TestRelay::start().await;
        relay.create_group("g");
        let (tx, _rx) = mpsc::unbounded_channel();
        let c = Connection::connect(&params(&relay, "a"), false, tx).await.unwrap();
        for t in ["sync_push", "sync_pull", "blob_put", "blob_get", "blob_release"] {
            let r = c.request_within(Frame::new(t), &["never"], Duration::from_secs(5)).await.err();
            assert_eq!(r, Some(TransportError::Relay("unsupported".into())), "{t}");
        }
        // The connection is still usable after an error reply.
        assert_eq!(c.peers().await.unwrap(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn peer_messages_reach_the_other_device_as_events() {
        let relay = crate::sync::test_relay::TestRelay::start().await;
        relay.create_group("g");
        let (tx_a, _rx_a) = mpsc::unbounded_channel();
        let a = Connection::connect(&params(&relay, "a"), false, tx_a).await.unwrap();
        assert_eq!(a.peers().await.unwrap(), Vec::<String>::new());
        let (tx_b, mut rx_b) = mpsc::unbounded_channel();
        let _b = Connection::connect(&params(&relay, "b"), false, tx_b).await.unwrap();
        assert_eq!(a.peers().await.unwrap(), vec!["b"]);

        a.peer_send("b", "c2VhbGVk".into()).await.unwrap();
        let ev = loop {
            match recv(&mut rx_b).await {
                Some(Event::Creator(_)) => continue,
                other => break other,
            }
        };
        assert_eq!(ev, Some(Event::PeerMsg { from: "a".into(), data: "c2VhbGVk".into() }));
        assert!(!format!("{ev:?}").contains("c2VhbGVk"), "data never printed");
        let offline = Some(TransportError::Relay("peer_offline".into()));
        assert_eq!(a.peer_send("z", "eA==".into()).await.err(), offline);
        assert_eq!(a.peer_send("a", "eA==".into()).await.err(), offline, "not to itself");
        let big = "x".repeat((4 * 1024 * 1024 + 64usize).div_ceil(3) * 4 + 1);
        assert_eq!(a.peer_send("b", big).await.err(), Some(TransportError::Relay("too_large".into())));
    }

    #[test]
    fn replies_match_by_echoed_id_first() {
        let (tx, _rx) = oneshot::channel();
        let w = Waiter { id: 7, expect: &["sync_peer_ack"], reply: tx };
        let mut f = Frame::new("sync_peer_ack");
        assert!(w.wants(&f), "no id: matched by type");
        f.id = Some(7.into());
        assert!(w.wants(&f));
        f.id = Some(6.into());
        assert!(!w.wants(&f), "stale reply for an earlier request");
        assert!(!w.wants(&Frame::new("sync_peers")));
        assert!(w.wants(&Frame::new("error")));
    }

    #[test]
    fn fatal_codes() {
        assert!(is_fatal_code("device_revoked"));
        assert!(is_fatal_code("auth_failed"));
        assert!(is_fatal_code("ticket_required") && is_fatal_code("ticket_invalid") && is_fatal_code("group_exists"));
        assert!(is_fatal_code(GROUP_FULL));
        assert!(!is_fatal_code("rate_limited"));
    }
}
