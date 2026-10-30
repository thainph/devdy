//! WebSocket client for the relay's sync frames (design §9.2).
//!
//! A sync connection is separate from the Remote Control one but dials the same
//! relay URL with the same `RELAY_HOST_TOKEN`, TLS stack and liveness rules as
//! `remote::agent` (connect timeout, 15 s keepalive, 45 s silence watchdog,
//! wake-from-sleep re-dial). One [`Connection`] = one authenticated socket;
//! reconnecting is the engine's job.
//!
//! Requests are strictly sequential (one in flight per connection) and carry
//! an `id` the relay echoes on the reply or error, which is how replies are
//! matched; a reply without an `id` is matched by type. The unsolicited
//! `sync_notify` / `sync_group_info` frames (and the welcome's
//! `creator_device`) are surfaced as [`Event`]s. Payloads (`blob`,
//! `data`) and tokens are never logged.

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

/// One item of `sync_push.items` / `sync_ops.items`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WireItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub blob: String,
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<WireItem>>,
    /// `sync_push`: the pusher's pull cursor; the relay only drops older
    /// same-key ops at or below it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub more: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shas: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released: Option<u64>,
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
    /// `sync_hello`: the relay store epoch this device last synced in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_epoch: Option<String>,
    /// `sync_welcome`: the relay store's current epoch (absent on old relays).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch: Option<String>,
    /// `sync_welcome`: the head the store recorded for `known_epoch` (absent:
    /// not asked, or an epoch this store never had).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_head: Option<u64>,
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
            .field("items", &self.items.as_ref().map(Vec::len))
            .field("last_seq", &self.last_seq)
            .field("more", &self.more)
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
    )
}

/// [`Connection::check_history`]'s code: the relay's log lacks ops this
/// device saw (needs a re-merge, `sync_rebootstrap`).
pub const RELAY_BEHIND: &str = "relay_behind";

/// What this device knows of the relay store's history: the store epoch it
/// last synced in and the highest seq it saw there (pulled, or acked for its
/// own push). See the relay's `new_store_epoch`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayMark {
    pub epoch: String,
    pub seen: u64,
}

/// Longest join ticket lifetime the relay accepts.
#[allow(dead_code)] // pairing (join tickets)
pub const MAX_TICKET_TTL_SECS: u64 = 600;

/// Unsolicited things the connection reports to its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Notify(u64),
    /// The group's creator per the relay (on welcome, and when it moves).
    Creator(String),
    Closed,
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
    /// The group's `last_seq` the relay announced in `sync_welcome`.
    head: u64,
    /// The store epoch from `sync_welcome` (`None`: a relay without epochs).
    epoch: Option<String>,
    /// The head the store recorded for the epoch the hello presented.
    known_head: Option<u64>,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Connection {
    /// Dial, send `sync_hello`, wait for `sync_welcome`. Events (notify,
    /// closed) go to `events` for the lifetime of the connection.
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
        Self::dial(params, create, ticket, None, events).await
    }

    /// [`Connection::connect`] presenting the store epoch this device last
    /// synced in, so [`Connection::check_history`] can tell a restored store.
    pub async fn connect_known(
        params: &ConnectParams,
        known_epoch: Option<&str>,
        events: mpsc::UnboundedSender<Event>,
    ) -> Result<Connection, TransportError> {
        Self::dial(params, false, None, known_epoch, events).await
    }

    async fn dial(
        params: &ConnectParams,
        create: bool,
        ticket: Option<&str>,
        known_epoch: Option<&str>,
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
        hello.known_epoch = known_epoch.map(str::to_string);
        send_frame(&mut sink, &hello).await?;

        // Wait for the hello verdict, ignoring anything else (keepalive echoes).
        let (head, epoch, known_head) = tokio::time::timeout(REQUEST_TIMEOUT, async {
            while let Some(msg) = stream.next().await {
                let Some(frame) = parse(msg.map_err(|e| TransportError::Network(e.to_string()))?) else {
                    continue;
                };
                match frame.t.as_str() {
                    "sync_welcome" => {
                        if let Some(c) = frame.creator_device.filter(|c| !c.is_empty()) {
                            let _ = events.send(Event::Creator(c));
                        }
                        return Ok((frame.last_seq.unwrap_or(0), frame.epoch.filter(|e| !e.is_empty()), frame.known_head));
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
                            "sync_notify" => {
                                let _ = events.send(Event::Notify(frame.last_seq.unwrap_or(0)));
                            }
                            "sync_group_info" => {
                                if let Some(c) = frame.creator_device.filter(|c| !c.is_empty()) {
                                    let _ = events.send(Event::Creator(c));
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
            head,
            epoch,
            known_head,
        })
    }

    /// N15 / M6: a relay that lost ops this device saw (restored from a
    /// backup) hands their seqs out again: pulling on would skip them, and
    /// pushing with the cursor as `base_seq` would let it drop ops this
    /// device never saw. Sync must stop until the device merges again.
    ///
    /// `mark` is what this device recorded at its last sync (connected with
    /// [`Connection::connect_known`]). Lost ops show as a head below what was
    /// seen, or — once other devices pushed the head past it again — as a
    /// store that does not know the device's epoch or recorded a lower head
    /// for it. Returns the mark to record from now on (`None` on a relay
    /// without epochs, where only the head can be compared).
    pub fn check_history(&self, cursor: u64, mark: Option<&RelayMark>) -> Result<Option<RelayMark>, TransportError> {
        let behind = || Err(TransportError::Rejected(RELAY_BEHIND.into()));
        let seen = mark.map_or(cursor, |m| m.seen.max(cursor));
        if self.head < seen {
            return behind();
        }
        if let (Some(m), Some(_)) = (mark, &self.epoch) {
            if self.known_head.is_none_or(|h| h < m.seen) {
                return behind();
            }
        }
        Ok(self.epoch.clone().map(|epoch| RelayMark { epoch, seen }))
    }

    /// Send `frame` and wait for its reply (one of `expect`, or an `error`).
    async fn request(&self, frame: Frame, expect: &'static [&'static str]) -> Result<Frame, TransportError> {
        self.request_within(frame, expect, REQUEST_TIMEOUT).await
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

    /// `sync_push` → the relay's `last_seq` after storing the items.
    /// `base_seq` is this device's pull cursor: everything up to it was
    /// applied before these ops were built.
    pub async fn push(&self, items: Vec<WireItem>, base_seq: u64) -> Result<u64, TransportError> {
        let mut f = Frame::new("sync_push");
        f.items = Some(items);
        f.base_seq = Some(base_seq);
        let ack = self.request(f, &["sync_ack"]).await?;
        Ok(ack.last_seq.unwrap_or(0))
    }

    /// `sync_pull` → `(items, last_seq, more)`.
    pub async fn pull(&self, after_seq: u64, limit: u64) -> Result<(Vec<WireItem>, u64, bool), TransportError> {
        let mut f = Frame::new("sync_pull");
        f.after_seq = Some(after_seq);
        f.limit = Some(limit);
        let ops = self.request(f, &["sync_ops"]).await?;
        Ok((
            ops.items.unwrap_or_default(),
            ops.last_seq.unwrap_or(0),
            ops.more.unwrap_or(false),
        ))
    }

    /// `blob_put` of already-sealed bytes (base64 `data`). Used by file sync.
    #[allow(dead_code)]
    pub async fn blob_put(&self, sha: &str, data_b64: String) -> Result<(), TransportError> {
        let mut f = Frame::new("blob_put");
        f.sha = Some(sha.to_string());
        f.data = Some(data_b64);
        self.request(f, &["blob_ack"]).await.map(|_| ())
    }

    /// `blob_get` → base64 sealed bytes; `Relay("blob_not_found")` when absent.
    #[allow(dead_code)]
    pub async fn blob_get(&self, sha: &str) -> Result<String, TransportError> {
        let mut f = Frame::new("blob_get");
        f.sha = Some(sha.to_string());
        let r = self.request(f, &["blob_data"]).await?;
        r.data.ok_or_else(|| TransportError::Relay("bad_blob".into()))
    }

    /// `blob_release{shas}` → how many the relay marked for deletion.
    pub async fn blob_release(&self, shas: &[String]) -> Result<u64, TransportError> {
        let mut f = Frame::new("blob_release");
        f.shas = Some(shas.to_vec());
        let r = self.request(f, &["blob_released"]).await?;
        Ok(r.released.unwrap_or(0))
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
        let mut f = Frame::new("sync_pull");
        f.after_seq = Some(4);
        f.limit = Some(500);
        assert_eq!(
            serde_json::to_value(&f).unwrap(),
            serde_json::json!({ "t": "sync_pull", "after_seq": 4, "limit": 500 })
        );
        let ops: Frame = serde_json::from_str(
            r#"{"t":"sync_ops","items":[{"seq":5,"device_id":"d","blob":"AA=="}],"last_seq":5,"more":false,"room_id":""}"#,
        )
        .unwrap();
        let items = ops.items.unwrap();
        assert_eq!(items[0].seq, Some(5));
        assert_eq!(items[0].blob, "AA==");
    }

    #[test]
    fn debug_never_prints_secrets() {
        let mut f = Frame::new("sync_hello");
        f.auth_token = Some("relay-secret".into());
        f.device_token = Some("device-secret".into());
        f.items = Some(vec![WireItem { blob: "sealed-blob".into(), ..Default::default() }]);
        let s = format!("{f:?}");
        assert!(!s.contains("secret") && !s.contains("sealed-blob"), "{s}");
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
    async fn a_relay_behind_the_cursor_is_refused() {
        let relay = crate::sync::test_relay::TestRelay::start().await;
        relay.create_group("g");
        relay.inject("g", "x", "b1");
        relay.inject("g", "x", "b2");
        let (tx, _rx) = mpsc::unbounded_channel();
        let conn = Connection::connect(&params(&relay, "d"), false, tx).await.unwrap();
        let behind = Err(TransportError::Rejected(RELAY_BEHIND.into()));
        assert!(conn.check_history(2, None).is_ok());
        assert!(conn.check_history(0, None).is_ok());
        // Restored from a backup older than what this device pulled.
        assert_eq!(conn.check_history(7, None), behind);
    }

    /// Review-3 M6: the relay is restored from a backup, then another device
    /// pushes its head past this device's cursor before this device comes
    /// back. The head alone no longer shows the loss; the epoch does. A plain
    /// relay restart is not mistaken for a restore.
    #[tokio::test]
    async fn a_restored_relay_is_refused_even_after_its_head_moved_on() {
        let relay = crate::sync::test_relay::TestRelay::start().await;
        relay.create_group("g");
        relay.inject("g", "x", "b1");
        let backup = relay.snapshot("g");
        relay.inject("g", "x", "b2");
        relay.inject("g", "x", "b3");
        let connect = |mark: Option<RelayMark>| {
            let p = params(&relay, "d");
            async move {
                let (tx, _rx) = mpsc::unbounded_channel();
                let conn = Connection::connect_known(&p, mark.as_ref().map(|m| m.epoch.as_str()), tx).await.unwrap();
                conn.check_history(3, mark.as_ref())
            }
        };
        // This device pulled up to 3.
        let mark = connect(None).await.unwrap().expect("the relay has epochs");
        assert_eq!(mark.seen, 3);
        // A restart keeps the history: accepted, the new epoch is adopted.
        relay.restart();
        let restarted = connect(Some(mark.clone())).await.unwrap().unwrap();
        assert_ne!(restarted.epoch, mark.epoch);
        assert_eq!(restarted.seen, 3);

        // Restored to seq 1; another device pushes the head to 5.
        relay.restore("g", backup);
        for b in ["n2", "n3", "n4", "n5"] {
            relay.inject("g", "y", b);
        }
        assert_eq!(relay.head(), 5);
        let behind = Err(TransportError::Rejected(RELAY_BEHIND.into()));
        assert_eq!(connect(Some(restarted)).await, behind, "epoch unknown to the restored store");
        // Restored within the epoch this device synced in: its head there is lower.
        let restored_mark = RelayMark { epoch: mark.epoch.clone(), seen: 3 };
        assert_eq!(connect(Some(restored_mark)).await, behind);
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
    ) -> (Connection, mpsc::UnboundedReceiver<Event>) {
        let ticket = format!("ticket-{device}");
        creator.sync_invite(&sha_hex(&ticket), 60).await.unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        let c = Connection::connect_with_ticket(&params(relay, device), false, Some(&ticket), tx).await.unwrap();
        (c, rx)
    }

    #[tokio::test]
    async fn revoke_kicks_the_device_and_blocks_its_hello() {
        let relay = crate::sync::test_relay::TestRelay::start().await;
        let (tx_a, _rx_a) = mpsc::unbounded_channel();
        let a = Connection::connect(&params(&relay, "a"), true, tx_a).await.unwrap();
        let (b, mut rx_b) = join(&relay, &a, "b").await;
        let (c, _rx_c) = join(&relay, &a, "c").await;
        // Only the creator revokes others.
        assert_eq!(c.revoke("b").await.err(), Some(TransportError::Rejected("forbidden".into())));
        drop(c);

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
        let (_b, mut rx_b) = join(&relay, &a, "b").await;
        let (c, mut rx_c) = join(&relay, &a, "c").await;
        assert_eq!(recv(&mut rx_b).await, Some(Event::Creator("a".into())));
        assert_eq!(recv(&mut rx_c).await, Some(Event::Creator("a".into())));

        let _ = a.revoke_with("a", Some("c")).await;
        assert_eq!(recv(&mut rx_b).await, Some(Event::Creator("c".into())));
        assert_eq!(recv(&mut rx_c).await, Some(Event::Creator("c".into())));
        c.revoke("b").await.unwrap();
    }

    #[tokio::test]
    async fn blobs_roundtrip_and_missing_is_a_relay_error() {
        let relay = crate::sync::test_relay::TestRelay::start().await;
        let (tx, _rx) = mpsc::unbounded_channel();
        let c = Connection::connect(&params(&relay, "a"), true, tx).await.unwrap();
        let keys = crate::sync::crypto::SyncKeys::derive(&[1u8; 32]).unwrap();
        let sealed = keys.seal_bytes(b"file body");
        let sha = crate::sync::crypto::blob_sha(&sealed);
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&sealed);
        c.blob_put(&sha, b64.clone()).await.unwrap();
        assert_eq!(c.blob_get(&sha).await.unwrap(), b64);
        assert_eq!(c.blob_get("00").await.err(), Some(TransportError::Relay("blob_not_found".into())));
        let mut push = Frame::new("sync_push");
        push.base_seq = Some(3);
        assert_eq!(serde_json::to_value(&push).unwrap(), serde_json::json!({ "t": "sync_push", "base_seq": 3 }));
        // The connection is still usable after an error reply.
        assert_eq!(c.pull(0, 10).await.unwrap().0.len(), 0);
    }

    #[test]
    fn replies_match_by_echoed_id_first() {
        let (tx, _rx) = oneshot::channel();
        let w = Waiter { id: 7, expect: &["sync_ack"], reply: tx };
        let mut f = Frame::new("sync_ack");
        assert!(w.wants(&f), "no id: matched by type");
        f.id = Some(7.into());
        assert!(w.wants(&f));
        f.id = Some(6.into());
        assert!(!w.wants(&f), "stale reply for an earlier request");
        assert!(!w.wants(&Frame::new("sync_ops")));
        assert!(w.wants(&Frame::new("error")));
    }

    #[test]
    fn fatal_codes() {
        assert!(is_fatal_code("device_revoked"));
        assert!(is_fatal_code("auth_failed"));
        assert!(is_fatal_code("ticket_required") && is_fatal_code("ticket_invalid") && is_fatal_code("group_exists"));
        assert!(!is_fatal_code("rate_limited"));
    }
}
