//! Relay connection, on-demand sync and device-to-device requests (design:
//! two-way-sync.md §3, §9.3; on-demand-sync.md §2).
//!
//! One task per configured group keeps a relay connection up (reconnect with
//! capped backoff, like `remote::agent`) so this device can serve the other
//! devices, but nothing syncs on its own (O1): capture keeps filling the
//! outbox, and data only moves when a user presses "Sync from <device>":
//! - the receiver ([`EngineHandle::run_from`]) sends the source an
//!   [`OPS_FLUSH`] request, waits for its `ops_flushed{last_seq}`, then pulls
//!   until its cursor reaches `last_seq` and the relay has no `more`, applying
//!   through the [`SyncStore`] (pulled `file` / `secret` ops go to
//!   `apply_remote_extra`, everything else to `apply_remote`). It pushes
//!   nothing of its own.
//! - the source serves `ops_flush`: it lets the store `stage` its non-DB
//!   sources (file chunks go up as sealed blobs through [`RelayBlobs`]),
//!   pushes every pending outbox op with the pull cursor as `base_seq`,
//!   releases superseded blobs and replies. An op the relay can never take
//!   (larger than [`MAX_OP_BLOB_CHARS`], or refused as `too_large` /
//!   `bad_request`) is parked (`SyncStore::park`) instead of blocking the
//!   queue; a refused blob upload (quota) holds the push back except for run
//!   ownership / lease rows ([`PRIORITY_TABLES`]) and fails the request.
//!
//! Requests travel as sealed [`PeerMsg`]s (`sync_peer_send` /
//! `sync_peer_msg`); the engine implements [`PeerLink`] over the live
//! connection and dispatches incoming requests to the registered
//! [`PeerHandler`]s (paused = requests are refused with [`PEER_PAUSED`]). The
//! socket only carries one request at a time: incoming messages arrive as
//! connection events and every request stays short, so a peer reply never
//! waits on a long transfer.
//!
//! On every (re)connect the store retries the pulled files / Keychain items
//! it could not fetch yet (left by the first-join merge or an earlier pull).
//! Status is published through a callback (the app emits it as
//! `sync://status`). Op contents, peer messages, blobs and keys are never
//! logged.

use super::crypto::{blob_sha, SyncKeys};
use super::op::Op;
use super::peer::{PeerHandler, PeerLink, PeerMsg, Replies, Reply, ERROR_KIND, MAX_BLOB_PLAINTEXT};
use super::store::{is_extra_tbl, BlobIo, StageScope, SyncStore};
use super::transport::{ConnectParams, Connection, Event, TransportError, WireItem};
use async_trait::async_trait;
use base64::Engine as _;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch, Notify};

pub const STATUS_EVENT: &str = "sync://status";
/// Progress of a manual sync (`{phase, message?}`).
pub const RUN_EVENT: &str = "sync://run";
/// Request served by the engine: push the whole outbox.
pub const OPS_FLUSH: &str = "ops_flush";
/// Reply to [`OPS_FLUSH`]: `{last_seq}`, the relay seq to pull up to.
pub const OPS_FLUSHED: &str = "ops_flushed";
/// Non-last reply while a flush runs: `{done}` ops pushed so far. Keeps the
/// requester's inactivity timeout from firing on a big backlog.
pub const OPS_FLUSH_PROGRESS: &str = "ops_flush_progress";
/// Request served by the engine: "you are the leader now — sync from me". The
/// receiver runs a sync from the sender, then replies [`LEADER_SYNCED`].
pub const LEADER_CHANGED: &str = "leader_changed";
pub const LEADER_SYNCED: &str = "leader_synced";
/// Non-last reply of long requests served by the engine (no body).
pub const STILL_WORKING: &str = "still_working";
/// Too many requests of that device in flight here.
pub const PEER_BUSY: &str = "busy";
/// The requester gave up on its request `reply_to` (dropped the replies or
/// timed out): the source stops serving it.
pub const PEER_CANCEL: &str = "cancel";
/// Requests of one device served at once; more are answered [`PEER_BUSY`].
const MAX_SERVED_PER_PEER: usize = 4;
/// The other device has no live relay connection (the relay's code).
#[allow(dead_code)] // matched by the UI; tests
pub const PEER_OFFLINE: &str = "peer_offline";
/// The other device paused sync: it serves no requests.
pub const PEER_PAUSED: &str = "peer_paused";
/// No reply within [`Timing::peer_timeout`].
pub const PEER_TIMEOUT: &str = "peer_timeout";
/// This device is not connected to the relay right now.
pub const NOT_CONNECTED: &str = "sync_not_connected";
const PEER_MSG_VERSION: u32 = 1;
const PUSH_BATCH: usize = 500;
const PUSH_BATCH_BYTES: usize = 4 * 1024 * 1024;
/// Rows pushed even while a refused blob upload holds the rest back
/// (review-3 M1): run ownership and lease renewals. Small, and each
/// supersedes its previous op.
const PRIORITY_TABLES: &[&str] = &["runs"];
/// Largest sealed op (base64 chars) worth sending: the relay's default cap,
/// base64 of `RELAY_SYNC_MAX_BLOB_BYTES` (4 MiB + 64). Bigger ops are parked.
pub const MAX_OP_BLOB_CHARS: usize = (4 * 1024 * 1024 + 64usize).div_ceil(3) * 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncState {
    Disabled,
    Connecting,
    Online,
    Offline,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncStatus {
    pub state: SyncState,
    pub paused: bool,
    /// Unix millis of the last push (serving a flush) / pull (a manual sync).
    pub last_push: Option<i64>,
    pub last_pull: Option<i64>,
    /// Ops waiting to be pushed as of the last flush (capped at one push batch).
    pub pending: usize,
    /// Rows whose latest change the relay permanently refused (not synced
    /// until they change again).
    pub parked: u64,
    pub last_error: Option<String>,
}

impl SyncStatus {
    pub fn disabled() -> Self {
        SyncStatus {
            state: SyncState::Disabled,
            paused: false,
            last_push: None,
            last_pull: None,
            pending: 0,
            parked: 0,
            last_error: None,
        }
    }
}

/// Phases of a manual sync, as emitted on [`RUN_EVENT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    Requesting,
    /// The source is pushing its outbox (message: ops pushed so far).
    Flushing,
    Pulling,
    Applying,
    Done,
    Error,
}

/// Outcome of [`EngineHandle::run_from`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SyncRunReport {
    /// Ops of other devices pulled.
    pub pulled: usize,
    pub applied: usize,
    /// The relay seq the source's flush reached.
    pub last_seq: u64,
}

/// Cadences, overridable by tests.
#[derive(Clone)]
pub struct Timing {
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    /// After a fatal rejection (revoked, bad token) retry this rarely.
    pub rejected_retry: Duration,
    /// Cap of the exponential backoff (from `backoff_min`) during which blob
    /// uploads through the [`PeerLink`] fail fast after the relay refused one.
    pub push_backoff_max: Duration,
    pub pull_limit: u64,
    /// A request fails after this long without any reply (each one resets it).
    pub peer_timeout: Duration,
    /// How often a long request served here sends a non-last reply.
    pub peer_keepalive: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(15),
            rejected_retry: Duration::from_secs(300),
            push_backoff_max: Duration::from_secs(600),
            pull_limit: 500,
            peer_timeout: Duration::from_secs(120),
            peer_keepalive: Duration::from_secs(15),
        }
    }
}

impl Timing {
    /// Short cadences for tests.
    #[cfg(test)]
    pub fn fast() -> Timing {
        Timing {
            backoff_min: Duration::from_millis(50),
            backoff_max: Duration::from_millis(200),
            rejected_retry: Duration::from_secs(3600),
            push_backoff_max: Duration::from_millis(400),
            pull_limit: 500,
            peer_timeout: Duration::from_secs(10),
            peer_keepalive: Duration::from_secs(2),
        }
    }
}

pub type StatusSink = Arc<dyn Fn(&SyncStatus) + Send + Sync>;

/// Handlers of incoming requests; shared with the app so features can
/// register theirs before or after the engine starts.
pub type PeerHandlers = Arc<RwLock<Vec<Arc<dyn PeerHandler>>>>;

/// A request this device sent, waiting for replies.
struct Pending {
    /// Only this device may answer it.
    peer: String,
    tx: mpsc::UnboundedSender<PeerMsg>,
}

/// Blob uploads through the link the relay refused (quota): fail fast for
/// a while instead of sending every chunk to be refused.
#[derive(Default)]
struct BlobRefusal {
    backoff: PushBackoff,
    code: Option<String>,
}

struct Shared {
    status: Mutex<SyncStatus>,
    sink: StatusSink,
    /// Retry connecting now.
    kick: Notify,
    paused: watch::Sender<bool>,
    /// The live connection, if any.
    conn: watch::Sender<Option<Arc<Connection>>>,
    /// Requests sent, by message id.
    pending: Mutex<HashMap<String, Pending>>,
    handlers: PeerHandlers,
    /// Serialises local store work: serving a flush, pulling for a sync. Never
    /// held while waiting on another device.
    work: tokio::sync::Mutex<()>,
    blob_refusal: Mutex<BlobRefusal>,
    /// Ops pushed by the flush in progress (progress replies).
    flush_done: AtomicU64,
    /// Requests being served, per requesting device.
    serving: Mutex<HashMap<String, usize>>,
    /// Tasks serving requests, by (requester, request id), to abort them.
    served: Mutex<HashMap<(String, String), tokio::task::AbortHandle>>,
}

impl Shared {
    fn new(sink: StatusSink, paused: bool, handlers: PeerHandlers) -> Self {
        let mut status = SyncStatus::disabled();
        status.paused = paused;
        Shared {
            status: Mutex::new(status),
            sink,
            kick: Notify::new(),
            paused: watch::channel(paused).0,
            conn: watch::channel(None).0,
            pending: Mutex::new(HashMap::new()),
            handlers,
            work: tokio::sync::Mutex::new(()),
            blob_refusal: Mutex::new(BlobRefusal::default()),
            flush_done: AtomicU64::new(0),
            serving: Mutex::new(HashMap::new()),
            served: Mutex::new(HashMap::new()),
        }
    }

    fn update(&self, f: impl FnOnce(&mut SyncStatus)) {
        let snapshot = {
            let Ok(mut s) = self.status.lock() else { return };
            f(&mut s);
            s.clone()
        };
        (self.sink)(&snapshot);
    }

    fn conn(&self) -> Option<Arc<Connection>> {
        self.conn.borrow().clone()
    }

    fn paused(&self) -> bool {
        *self.paused.borrow()
    }
}

/// Handle to a running engine. Dropping it does not stop the task; call
/// [`SyncEngine::stop`].
pub struct SyncEngine {
    handle: EngineHandle,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

/// What callers can do with a running engine; cheap to clone, so long
/// operations do not hold whatever holds the [`SyncEngine`].
#[derive(Clone)]
pub struct EngineHandle {
    ctx: Arc<Ctx>,
}

impl SyncEngine {
    #[cfg(test)]
    pub fn start(
        params: ConnectParams,
        keys: SyncKeys,
        store: Arc<dyn SyncStore>,
        sink: StatusSink,
        paused: bool,
        timing: Timing,
    ) -> SyncEngine {
        Self::start_with(params, keys, store, sink, paused, timing, PeerHandlers::default())
    }

    /// [`SyncEngine::start`] serving requests with `handlers`.
    pub fn start_with(
        params: ConnectParams,
        keys: SyncKeys,
        store: Arc<dyn SyncStore>,
        sink: StatusSink,
        paused: bool,
        timing: Timing,
        handlers: PeerHandlers,
    ) -> SyncEngine {
        let ctx = Arc::new(Ctx {
            params,
            keys,
            store,
            shared: Arc::new(Shared::new(sink, paused, handlers)),
            timing,
        });
        let (stop_tx, stop_rx) = watch::channel(false);
        let task = tokio::spawn(run(ctx.clone(), stop_rx));
        SyncEngine {
            handle: EngineHandle { ctx },
            stop: stop_tx,
            task,
        }
    }

    pub fn handle(&self) -> EngineHandle {
        self.handle.clone()
    }

    pub fn status(&self) -> SyncStatus {
        self.handle.status()
    }

    /// Reconnect right away when waiting to retry.
    pub fn kick(&self) {
        self.handle.ctx.shared.kick.notify_one();
    }

    /// Parked rows changed outside a flush (retry): show the new count now.
    pub fn set_parked(&self, parked: u64) {
        self.handle.ctx.shared.update(|s| s.parked = parked);
    }

    /// Paused: the connection stays up but requests of other devices are
    /// refused with [`PEER_PAUSED`].
    pub fn set_paused(&self, paused: bool) {
        let shared = &self.handle.ctx.shared;
        shared.paused.send_replace(paused);
        shared.update(|s| s.paused = paused);
    }

    pub async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(2), self.task).await;
    }
}

impl EngineHandle {
    pub fn status(&self) -> SyncStatus {
        self.ctx
            .shared
            .status
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| SyncStatus::disabled())
    }

    /// Requests, replies and blobs to / from the other devices.
    pub fn link(&self) -> Arc<dyn PeerLink> {
        Arc::new(EngineLink(self.ctx.clone()))
    }

    /// Devices of the group connected to the relay right now (not this one).
    pub async fn online_peers(&self) -> Result<Vec<String>, String> {
        self.link().online_peers().await
    }

    /// Tell `to`, the new leader, to sync from this device right away; Ok
    /// once it did.
    pub async fn notify_leader(&self, to: &str) -> Result<(), String> {
        let mut replies = request(&self.ctx, to, LEADER_CHANGED, json!({})).await?;
        loop {
            match replies.recv().await {
                Some(Ok((kind, _))) if kind == LEADER_SYNCED => return Ok(()),
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(e),
                None => return Err("peer connection lost".into()),
            }
        }
    }

    /// Push this device's whole outbox now (what serving [`OPS_FLUSH`] does);
    /// returns the relay seq it reached.
    pub async fn flush_outbox(&self, scope: StageScope) -> Result<u64, String> {
        flush(&self.ctx, scope).await
    }

    /// Manual sync from `source`: it flushes its outbox (uploading the files
    /// `scope` asks for), this device pulls and applies. `progress` sees the
    /// non-final phases.
    pub async fn run_from(
        &self,
        source: &str,
        scope: StageScope,
        progress: &(dyn Fn(RunPhase, Option<String>) + Send + Sync),
    ) -> Result<SyncRunReport, String> {
        run_from(&self.ctx, source, scope, progress).await
    }
}

struct Ctx {
    params: ConnectParams,
    keys: SyncKeys,
    store: Arc<dyn SyncStore>,
    shared: Arc<Shared>,
    timing: Timing,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

async fn run(ctx: Arc<Ctx>, mut stop: watch::Receiver<bool>) {
    let mut backoff = ctx.timing.backoff_min;
    loop {
        if *stop.borrow() {
            break;
        }
        ctx.shared.update(|s| s.state = SyncState::Connecting);
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let wait = match connect_checked(&ctx, ev_tx).await {
            Ok(conn) => {
                backoff = ctx.timing.backoff_min;
                let conn = Arc::new(conn);
                ctx.shared.conn.send_replace(Some(conn.clone()));
                ctx.shared.update(|s| {
                    s.state = SyncState::Online;
                    s.last_error = None;
                });
                tracing::info!(event = "sync_connected");
                tokio::spawn(retry_deferred(ctx.clone()));
                serve(&ctx, ev_rx, &mut stop).await;
                ctx.shared.conn.send_replace(None);
                conn.close();
                // Replies to requests in flight are lost with the socket.
                if let Ok(mut p) = ctx.shared.pending.lock() {
                    p.clear();
                }
                if *stop.borrow() {
                    break;
                }
                ctx.shared.update(|s| s.state = SyncState::Offline);
                ctx.timing.backoff_min
            }
            Err(TransportError::Rejected(code)) => {
                tracing::warn!(event = "sync_rejected", code = %code);
                let msg = TransportError::Rejected(code).to_string();
                ctx.shared.update(|s| {
                    s.state = SyncState::Error;
                    s.last_error = Some(msg);
                });
                ctx.timing.rejected_retry
            }
            Err(e) => {
                tracing::info!(event = "sync_connect_failed", error = %e);
                let msg = e.to_string();
                ctx.shared.update(|s| {
                    s.state = SyncState::Offline;
                    s.last_error = Some(msg);
                });
                let w = backoff;
                backoff = (backoff * 2).min(ctx.timing.backoff_max);
                w
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = stop.changed() => {}
            _ = ctx.shared.kick.notified() => {}
        }
    }
    ctx.shared.update(|s| s.state = SyncState::Disabled);
}

/// Connect presenting the relay store epoch this device last synced in, and
/// refuse a relay that lost ops this device saw (restored from a backup):
/// sync stops until the device merges again (N15, review-3 M6).
async fn connect_checked(ctx: &Ctx, events: mpsc::UnboundedSender<Event>) -> Result<Connection, TransportError> {
    let store_err = TransportError::Network;
    let mark = ctx.store.relay_mark().await.map_err(store_err)?;
    let conn = Connection::connect_known(&ctx.params, mark.as_ref().map(|m| m.epoch.as_str()), events).await?;
    let cursor = ctx.store.cursor().await.map_err(store_err)?;
    if let Some(next) = conn.check_history(cursor, mark.as_ref())? {
        if Some(&next) != mark.as_ref() {
            ctx.store.set_relay_mark(&next).await.map_err(store_err)?;
        }
    }
    Ok(conn)
}

/// Handle the connection's events until it drops or we stop. Never blocks on
/// sync work: requests are served on their own tasks.
async fn serve(ctx: &Arc<Ctx>, mut events: mpsc::UnboundedReceiver<Event>, stop: &mut watch::Receiver<bool>) {
    loop {
        tokio::select! {
            _ = stop.changed() => return,
            ev = events.recv() => match ev {
                // On demand: another device's push does not trigger a pull.
                Some(Event::Notify(_)) => {}
                Some(Event::Creator(c)) => {
                    if let Err(e) = ctx.store.set_group_creator(&c).await {
                        tracing::warn!(event = "sync_creator_store_failed", error = %e);
                    }
                }
                Some(Event::PeerMsg { from, data }) => on_peer_msg(ctx, from, &data),
                Some(Event::Closed) | None => return,
            },
        }
    }
}

/// Files and Keychain items pulled earlier but not fetched yet (the
/// first-join merge leaves them to the engine): fetch them now.
async fn retry_deferred(ctx: Arc<Ctx>) {
    let Some(conn) = ctx.shared.conn() else { return };
    let _work = ctx.shared.work.lock().await;
    let blobs = RelayBlobs::new(&conn, &ctx.keys);
    match ctx.store.apply_remote_extra(Vec::new(), &blobs).await {
        Ok(r) if r.applied > 0 => tracing::debug!(event = "sync_deferred_applied", applied = r.applied),
        Ok(_) => {}
        Err(e) => tracing::debug!(event = "sync_deferred_retry_failed", error = %e),
    }
}

fn new_msg_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// A relay refusal as its bare code (`peer_offline`, `quota_exceeded`…).
fn code_of(e: TransportError) -> String {
    match e {
        TransportError::Relay(code) => code,
        other => other.to_string(),
    }
}

/// Without a running engine (first-join merge): ask every online device of
/// the group to push its whole outbox, one after the other, so the relay
/// holds the group's full state before it is read. `events` must be the
/// receiver of `conn`'s event channel. Returns the devices that flushed;
/// one that is offline, paused or failing is skipped (its rows arrive with
/// a later Sync from it).
pub async fn flush_online_peers(
    conn: &Connection,
    events: &mut mpsc::UnboundedReceiver<Event>,
    keys: &SyncKeys,
    scope: StageScope,
    idle: Duration,
) -> Result<Vec<String>, String> {
    let mut flushed = Vec::new();
    for peer in conn.peers().await.map_err(code_of)? {
        let msg = PeerMsg {
            v: PEER_MSG_VERSION,
            id: new_msg_id(),
            kind: OPS_FLUSH.to_string(),
            reply_to: None,
            last: false,
            body: json!({ "files": scope }),
        };
        let plain = serde_json::to_vec(&msg).map_err(|e| e.to_string())?;
        let data = base64::engine::general_purpose::STANDARD.encode(keys.seal_bytes(&plain));
        if let Err(e) = conn.peer_send(&peer, data).await {
            tracing::info!(event = "sync_join_flush_skipped", error = %code_of(e));
            continue;
        }
        // Progress replies keep it alive; anything else from the peer is ignored.
        let done = loop {
            match tokio::time::timeout(idle, events.recv()).await {
                Err(_) => break Err(PEER_TIMEOUT.to_string()),
                Ok(None) | Ok(Some(Event::Closed)) => return Err("peer connection lost".into()),
                Ok(Some(Event::PeerMsg { from, data })) if from == peer => {
                    let Some(reply) = open_peer_msg(keys, &data) else { continue };
                    if reply.reply_to.as_deref() != Some(msg.id.as_str()) {
                        continue;
                    }
                    if reply.kind == OPS_FLUSHED {
                        break Ok(());
                    }
                    if reply.kind == ERROR_KIND {
                        let m = reply.body.get("message").and_then(Value::as_str).unwrap_or("error");
                        break Err(m.to_string());
                    }
                }
                Ok(Some(_)) => {}
            }
        };
        match done {
            Ok(()) => flushed.push(peer),
            Err(e) => tracing::info!(event = "sync_join_flush_skipped", error = %e),
        }
    }
    Ok(flushed)
}

fn open_peer_msg(keys: &SyncKeys, data: &str) -> Option<PeerMsg> {
    let sealed = base64::engine::general_purpose::STANDARD.decode(data).ok()?;
    let plain = keys.open_bytes(&sealed).ok()?;
    serde_json::from_slice(&plain).ok()
}

/// Seal `msg` with the group key and hand it to the relay for `to`.
async fn send_peer(ctx: &Ctx, to: &str, msg: &PeerMsg) -> Result<(), String> {
    let conn = ctx.shared.conn().ok_or(NOT_CONNECTED)?;
    let plain = serde_json::to_vec(msg).map_err(|e| e.to_string())?;
    let data = base64::engine::general_purpose::STANDARD.encode(ctx.keys.seal_bytes(&plain));
    if data.len() > MAX_OP_BLOB_CHARS {
        return Err("too_large".into());
    }
    conn.peer_send(to, data).await.map_err(code_of)
}

/// A message from another device: a reply goes to the request waiting for
/// it, a request is served on its own task.
fn on_peer_msg(ctx: &Arc<Ctx>, from: String, data: &str) {
    // Anything that does not open with the group key is dropped.
    let Some(msg) = open_peer_msg(&ctx.keys, data) else {
        tracing::warn!(event = "sync_peer_msg_invalid");
        return;
    };
    if msg.kind == PEER_CANCEL {
        if let Some(id) = msg.reply_to.as_deref() {
            abort_served(ctx, &from, id);
        }
        return;
    }
    if let Some(to) = msg.reply_to.clone() {
        let Ok(mut pending) = ctx.shared.pending.lock() else { return };
        match pending.get(&to) {
            Some(p) if p.peer == from => {
                let end = msg.last || msg.kind == ERROR_KIND;
                if p.tx.send(msg).is_err() || end {
                    pending.remove(&to);
                }
            }
            _ => tracing::debug!(event = "sync_peer_reply_unmatched"),
        }
        return;
    }
    let ctx = ctx.clone();
    let Some(slot) = ServingSlot::take(&ctx, &from) else {
        tracing::info!(event = "sync_peer_busy");
        let reply = PeerReply { ctx: ctx.clone(), to: from, reply_to: msg.id };
        tokio::spawn(async move {
            let reply: Arc<dyn Reply> = Arc::new(reply);
            let _ = reply.error(PEER_BUSY).await;
        });
        return;
    };
    let key = (from.clone(), msg.id.clone());
    let entry = ServedEntry { ctx: ctx.clone(), key: key.clone() };
    // Registered under the lock, so a task ending at once still finds (and
    // removes) its entry. Aborting drops the slot and the entry with the task.
    let shared = ctx.shared.clone();
    let Ok(mut served) = shared.served.lock() else { return };
    let task = tokio::spawn(async move {
        let _held = (slot, entry);
        serve_request(ctx, from, msg).await;
    });
    served.insert(key, task.abort_handle());
}

/// Stop serving `from`'s request `id` (it gave up, or cannot be reached).
fn abort_served(ctx: &Ctx, from: &str, id: &str) {
    let handle = ctx
        .shared
        .served
        .lock()
        .ok()
        .and_then(|mut s| s.remove(&(from.to_string(), id.to_string())));
    if let Some(h) = handle {
        tracing::info!(event = "sync_peer_request_aborted");
        h.abort();
    }
}

/// A served request's entry in `Shared::served`; removed on drop.
struct ServedEntry {
    ctx: Arc<Ctx>,
    key: (String, String),
}

impl Drop for ServedEntry {
    fn drop(&mut self) {
        if let Ok(mut s) = self.ctx.shared.served.lock() {
            s.remove(&self.key);
        }
    }
}

/// One of the [`MAX_SERVED_PER_PEER`] requests a device may have served here
/// at once; freed on drop.
struct ServingSlot {
    ctx: Arc<Ctx>,
    from: String,
}

impl ServingSlot {
    fn take(ctx: &Arc<Ctx>, from: &str) -> Option<ServingSlot> {
        let mut serving = ctx.shared.serving.lock().ok()?;
        let n = serving.entry(from.to_string()).or_default();
        if *n >= MAX_SERVED_PER_PEER {
            return None;
        }
        *n += 1;
        Some(ServingSlot { ctx: ctx.clone(), from: from.to_string() })
    }
}

impl Drop for ServingSlot {
    fn drop(&mut self) {
        if let Ok(mut serving) = self.ctx.shared.serving.lock() {
            if let Some(n) = serving.get_mut(&self.from) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    serving.remove(&self.from);
                }
            }
        }
    }
}

/// Run `work`, sending a non-last `kind` reply with `body()` every
/// [`Timing::peer_keepalive`] until it ends, so the requester does not time out.
///
/// The sends run as their own future, polled alongside `work`: a send waits
/// for the connection's request lock, which `work` may hold across a request,
/// so awaiting it while `work` is not polled would deadlock.
async fn with_keepalive<T>(
    ctx: &Ctx,
    reply: &Arc<dyn Reply>,
    kind: &str,
    body: impl Fn() -> Value,
    work: impl std::future::Future<Output = T>,
) -> T {
    let every = ctx.timing.peer_keepalive;
    let keepalive = async {
        let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
        loop {
            tick.tick().await;
            if let Err(e) = reply.send(kind, body(), false).await {
                tracing::debug!(event = "sync_peer_keepalive_failed", error = %e);
            }
        }
    };
    tokio::pin!(work, keepalive);
    tokio::select! {
        out = &mut work => out,
        never = &mut keepalive => never,
    }
}

async fn serve_request(ctx: Arc<Ctx>, from: String, msg: PeerMsg) {
    tracing::info!(event = "sync_peer_request", kind = %msg.kind);
    let reply: Arc<dyn Reply> = Arc::new(PeerReply {
        ctx: ctx.clone(),
        to: from.clone(),
        reply_to: msg.id.clone(),
    });
    let sent = if ctx.shared.paused() {
        reply.error(PEER_PAUSED).await
    } else if msg.kind == OPS_FLUSH {
        let done = || json!({ "done": ctx.shared.flush_done.load(Ordering::Relaxed) });
        // Older devices send no scope: everything, as before.
        let scope: StageScope = msg.body.get("files").cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
        match with_keepalive(&ctx, &reply, OPS_FLUSH_PROGRESS, done, flush(&ctx, scope)).await {
            Ok(last_seq) => reply.send(OPS_FLUSHED, json!({ "last_seq": last_seq }), true).await,
            Err(e) => reply.error(e).await,
        }
    } else if msg.kind == LEADER_CHANGED {
        // The sender made this device the leader: fetch that now, so its
        // singleton jobs do not wait for the next manual sync.
        // Only the leader setting matters here: rows, no files.
        let synced = run_from(&ctx, &from, StageScope::Nothing, &|_, _| {});
        match with_keepalive(&ctx, &reply, STILL_WORKING, || json!({}), synced).await {
            Ok(_) => reply.send(LEADER_SYNCED, json!({}), true).await,
            Err(e) => reply.error(e).await,
        }
    } else {
        let handler = ctx
            .shared
            .handlers
            .read()
            .ok()
            .and_then(|hs| hs.iter().find(|h| h.kinds().contains(&msg.kind.as_str())).cloned());
        match handler {
            Some(h) => {
                h.handle(Arc::new(EngineLink(ctx.clone())), from, msg.kind, msg.body, reply).await;
                Ok(())
            }
            None => reply.error(format!("unknown request kind {}", msg.kind)).await,
        }
    };
    if let Err(e) = sent {
        tracing::info!(event = "sync_peer_reply_failed", error = %e);
    }
}

/// Replies to one incoming request.
struct PeerReply {
    ctx: Arc<Ctx>,
    to: String,
    reply_to: String,
}

#[async_trait]
impl Reply for PeerReply {
    async fn send(&self, kind: &str, body: Value, last: bool) -> Result<(), String> {
        let msg = PeerMsg {
            v: PEER_MSG_VERSION,
            id: new_msg_id(),
            kind: kind.to_string(),
            reply_to: Some(self.reply_to.clone()),
            last,
            body,
        };
        let sent = send_peer(&self.ctx, &self.to, &msg).await;
        if sent.as_ref().is_err_and(|e| e == PEER_OFFLINE) {
            // The requester is gone: stop working for it.
            abort_served(&self.ctx, &self.to, &self.reply_to);
        }
        sent
    }
}

/// Send a request; its replies arrive on the returned stream, which ends
/// after the `last` one, an error reply, [`Timing::peer_timeout`] without a
/// reply, or a lost connection.
async fn request(ctx: &Arc<Ctx>, to: &str, kind: &str, body: Value) -> Result<Replies, String> {
    let id = new_msg_id();
    let (in_tx, mut in_rx) = mpsc::unbounded_channel::<PeerMsg>();
    let pending = Pending { peer: to.to_string(), tx: in_tx };
    ctx.shared.pending.lock().map_err(|e| e.to_string())?.insert(id.clone(), pending);
    let msg = PeerMsg {
        v: PEER_MSG_VERSION,
        id: id.clone(),
        kind: kind.to_string(),
        reply_to: None,
        last: false,
        body,
    };
    let forget = |ctx: &Ctx, id: &str| {
        if let Ok(mut p) = ctx.shared.pending.lock() {
            p.remove(id);
        }
    };
    if let Err(e) = send_peer(ctx, to, &msg).await {
        forget(ctx, &id);
        return Err(e);
    }
    let (out_tx, out_rx) = mpsc::channel(64);
    let (ctx, to) = (ctx.clone(), to.to_string());
    tokio::spawn(async move {
        // Whether the stream ended before the source's `last` reply while the
        // source may still be working on it.
        let gave_up = loop {
            let m = tokio::select! {
                r = tokio::time::timeout(ctx.timing.peer_timeout, in_rx.recv()) => match r {
                    Ok(Some(m)) => m,
                    Ok(None) => {
                        let _ = out_tx.send(Err("peer connection lost".into())).await;
                        break false;
                    }
                    Err(_) => {
                        let _ = out_tx.send(Err(PEER_TIMEOUT.into())).await;
                        break true;
                    }
                },
                // The requester dropped the replies.
                _ = out_tx.closed() => break true,
            };
            let end = m.last || m.kind == ERROR_KIND;
            let item = if m.kind == ERROR_KIND {
                Err(m.body.get("message").and_then(Value::as_str).unwrap_or("error").to_string())
            } else {
                Ok((m.kind, m.body))
            };
            if out_tx.send(item).await.is_err() {
                break !end;
            }
            if end {
                break false;
            }
        };
        forget(&ctx, &id);
        if gave_up {
            let cancel = PeerMsg {
                v: PEER_MSG_VERSION,
                id: new_msg_id(),
                kind: PEER_CANCEL.to_string(),
                reply_to: Some(id),
                last: true,
                body: Value::Null,
            };
            if let Err(e) = send_peer(&ctx, &to, &cancel).await {
                tracing::debug!(event = "sync_peer_cancel_failed", error = %e);
            }
        }
    });
    Ok(out_rx)
}

/// [`PeerLink`] over the engine's live connection.
struct EngineLink(Arc<Ctx>);

impl EngineLink {
    fn conn(&self) -> Result<Arc<Connection>, String> {
        self.0.shared.conn().ok_or_else(|| NOT_CONNECTED.to_string())
    }
}

#[async_trait]
impl PeerLink for EngineLink {
    async fn online_peers(&self) -> Result<Vec<String>, String> {
        self.conn()?.peers().await.map_err(code_of)
    }

    async fn request(&self, to: &str, kind: &str, body: Value) -> Result<Replies, String> {
        request(&self.0, to, kind, body).await
    }

    async fn blob_put(&self, plaintext: Vec<u8>) -> Result<String, String> {
        if plaintext.len() > MAX_BLOB_PLAINTEXT {
            return Err("too_large".into());
        }
        let shared = &self.0.shared;
        if let Ok(r) = shared.blob_refusal.lock() {
            if let (false, Some(code)) = (r.backoff.due(), &r.code) {
                return Err(code.clone());
            }
        }
        let conn = self.conn()?;
        let sealed = self.0.keys.seal_bytes(&plaintext);
        let sha = blob_sha(&sealed);
        let data = base64::engine::general_purpose::STANDARD.encode(&sealed);
        match conn.blob_put(&sha, data).await {
            Ok(()) => {
                if let Ok(mut r) = shared.blob_refusal.lock() {
                    *r = BlobRefusal::default();
                }
                Ok(sha)
            }
            Err(TransportError::Relay(code)) => {
                let t = &self.0.timing;
                if let Ok(mut r) = shared.blob_refusal.lock() {
                    let wait = r.backoff.failed(t.backoff_min, t.push_backoff_max);
                    r.code = Some(code.clone());
                    tracing::info!(event = "sync_blob_backoff", secs = wait.as_secs());
                }
                let msg = TransportError::Relay(code.clone()).to_string();
                shared.update(|s| s.last_error = Some(msg));
                Err(code)
            }
            Err(e) => Err(e.to_string()),
        }
    }

    async fn blob_get(&self, sha: &str) -> Result<Vec<u8>, String> {
        let data = self.conn()?.blob_get(sha).await.map_err(code_of)?;
        open_blob(&self.0.keys, sha, data)
    }

    async fn blob_release(&self, shas: &[String]) -> Result<(), String> {
        let conn = self.conn()?;
        for chunk in shas.chunks(RELEASE_BATCH) {
            match conn.blob_release(chunk).await {
                // A relay without `blob_release`: the blobs simply stay.
                Ok(_) | Err(TransportError::Relay(_)) => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(())
    }
}

/// Serve [`OPS_FLUSH`]: stage, push the whole outbox, release superseded
/// blobs. Returns the relay seq the receiver has to pull up to.
async fn flush(ctx: &Ctx, scope: StageScope) -> Result<u64, String> {
    let conn = ctx.shared.conn().ok_or(NOT_CONNECTED)?;
    let _work = ctx.shared.work.lock().await;
    ctx.shared.flush_done.store(0, Ordering::Relaxed);
    let result = flush_on(ctx, &conn, scope).await;
    if let Ok(parked) = ctx.store.parked_count().await {
        ctx.shared.update(|s| s.parked = parked);
    }
    match result {
        Ok(seq) => {
            ctx.shared.update(|s| {
                s.state = SyncState::Online;
                s.last_error = None;
            });
            Ok(seq)
        }
        Err(e) => {
            let msg = e.to_string();
            tracing::info!(event = "sync_flush_failed", error = %msg);
            // What is still queued (a failure may come before or midway
            // through the push, which is what counts it).
            if let Ok(left) = ctx.store.pending(PUSH_BATCH).await {
                ctx.shared.update(|s| s.pending = left.len());
            }
            let dead = is_dead(&e);
            let shown = msg.clone();
            ctx.shared.update(|s| {
                if !dead {
                    s.state = SyncState::Error;
                }
                s.last_error = Some(shown);
            });
            Err(match e {
                // The bare code, so the requester can tell quota from the rest.
                CycleError::Transport(TransportError::Relay(code)) => code,
                _ => msg,
            })
        }
    }
}

async fn flush_on(ctx: &Ctx, conn: &Connection, scope: StageScope) -> Result<u64, CycleError> {
    match stage(ctx, conn, scope).await {
        // A refused upload (quota): the files cannot go out, so nothing
        // that may reference them does; lease renewals still do.
        Err(e @ CycleError::Transport(TransportError::Relay(_))) => {
            if let Err(p) = push_priority(ctx, conn).await {
                tracing::debug!(event = "sync_priority_push_failed", error = %p);
            }
            return Err(e);
        }
        // The rows still go out.
        Err(CycleError::Store(e)) => tracing::warn!(event = "sync_stage_failed", error = %e),
        Err(e) => return Err(e),
        Ok(()) => {}
    }
    let acked = match ctx.store.has_pending().await.map_err(CycleError::Store)? {
        true => push_all(ctx, conn).await?,
        false => {
            ctx.shared.update(|s| s.pending = 0);
            None
        }
    };
    release_blobs(ctx, conn).await?;
    let seen = ctx.store.relay_mark().await.ok().flatten().map_or(0, |m| m.seen);
    Ok(acked.unwrap_or(0).max(seen))
}

/// The receiver's side of a manual sync.
async fn run_from(
    ctx: &Arc<Ctx>,
    source: &str,
    scope: StageScope,
    progress: &(dyn Fn(RunPhase, Option<String>) + Send + Sync),
) -> Result<SyncRunReport, String> {
    if source == ctx.params.device_id {
        return Err("cannot sync from this device itself".into());
    }
    progress(RunPhase::Requesting, None);
    let mut replies = request(ctx, source, OPS_FLUSH, json!({ "files": scope })).await?;
    let last_seq = loop {
        match replies.recv().await {
            Some(Ok((kind, body))) if kind == OPS_FLUSHED => {
                break body.get("last_seq").and_then(Value::as_u64).unwrap_or(0);
            }
            Some(Ok((kind, body))) if kind == OPS_FLUSH_PROGRESS => {
                let done = body.get("done").and_then(Value::as_u64).unwrap_or(0);
                progress(RunPhase::Flushing, Some(done.to_string()));
            }
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(e),
            None => return Err("peer connection lost".into()),
        }
    };
    drop(replies);
    let conn = ctx.shared.conn().ok_or(NOT_CONNECTED)?;
    let _work = ctx.shared.work.lock().await;
    let blobs = RelayBlobs::new(&conn, &ctx.keys);
    let mut counts = pull_all(ctx, &conn, &blobs, progress).await.map_err(|e| e.to_string())?;
    if !counts.extra {
        // Files / Keychain items an earlier pull could not fetch.
        if let Ok(r) = ctx.store.apply_remote_extra(Vec::new(), &blobs).await {
            counts.applied += r.applied;
        }
    }
    let cursor = ctx.store.cursor().await?;
    // Selective sync turned a category / project on meanwhile: the cursor was
    // reset to 0 and the next sync pulls everything again.
    if cursor < last_seq && !ctx.store.cursor_reset_pending().await? {
        return Err(format!("the relay is behind the source (at {cursor}, flushed up to {last_seq})"));
    }
    Ok(SyncRunReport { pulled: counts.pulled, applied: counts.applied, last_seq })
}

/// Delay before uploading again after the relay refused an upload.
#[derive(Default)]
struct PushBackoff {
    delay: Option<Duration>,
    until: Option<Instant>,
}

impl PushBackoff {
    fn due(&self) -> bool {
        self.until.is_none_or(|t| Instant::now() >= t)
    }

    /// Back off `backoff_min`, then twice as long each time, up to `max`.
    fn failed(&mut self, min: Duration, max: Duration) -> Duration {
        let d = self.delay.map_or(min, |d| (d * 2).min(max));
        self.delay = Some(d);
        self.until = Some(Instant::now() + d);
        d
    }
}

#[derive(Debug)]
enum CycleError {
    Transport(TransportError),
    Store(String),
}

impl std::fmt::Display for CycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CycleError::Transport(t) => t.fmt(f),
            CycleError::Store(s) => f.write_str(s),
        }
    }
}

impl From<TransportError> for CycleError {
    fn from(e: TransportError) -> Self {
        CycleError::Transport(e)
    }
}

/// The connection is unusable (as opposed to one refused request).
fn is_dead(e: &CycleError) -> bool {
    matches!(e, CycleError::Transport(t) if !matches!(t, TransportError::Relay(_)))
}

/// Sealed relay blobs over this connection (file chunks).
pub struct RelayBlobs<'a> {
    conn: &'a Connection,
    keys: &'a SyncKeys,
    /// The first upload error (the store only sees its text).
    put_error: Mutex<Option<TransportError>>,
}

impl<'a> RelayBlobs<'a> {
    pub fn new(conn: &'a Connection, keys: &'a SyncKeys) -> Self {
        RelayBlobs { conn, keys, put_error: Mutex::new(None) }
    }

    fn take_put_error(&self) -> Option<TransportError> {
        self.put_error.lock().ok()?.take()
    }
}

/// Decode, check and open a blob the relay returned for `sha`.
fn open_blob(keys: &SyncKeys, sha: &str, data_b64: String) -> Result<Vec<u8>, String> {
    let sealed = base64::engine::general_purpose::STANDARD
        .decode(data_b64)
        .map_err(|_| "blob is not valid base64".to_string())?;
    if blob_sha(&sealed) != sha {
        return Err("blob does not match its sha".into());
    }
    keys.open_bytes(&sealed).map_err(|e| e.to_string())
}

#[async_trait]
impl BlobIo for RelayBlobs<'_> {
    async fn put(&self, plaintext: &[u8]) -> Result<String, String> {
        let sealed = self.keys.seal_bytes(plaintext);
        let sha = blob_sha(&sealed);
        let data = base64::engine::general_purpose::STANDARD.encode(&sealed);
        if let Err(e) = self.conn.blob_put(&sha, data).await {
            let msg = e.to_string();
            if let Ok(mut first) = self.put_error.lock() {
                first.get_or_insert(e);
            }
            return Err(msg);
        }
        Ok(sha)
    }

    async fn get(&self, sha: &str) -> Result<Vec<u8>, String> {
        let data = self.conn.blob_get(sha).await.map_err(|e| e.to_string())?;
        open_blob(self.keys, sha, data)
    }
}

/// Let the store stage its files. A blob upload that failed on the relay's
/// side comes back as that transport error (a refusal holds the push back,
/// a dead socket fails the flush).
async fn stage(ctx: &Ctx, conn: &Connection, scope: StageScope) -> Result<(), CycleError> {
    let blobs = RelayBlobs::new(conn, &ctx.keys);
    let staged = ctx.store.stage_scoped(&blobs, scope).await;
    match (blobs.take_put_error(), staged) {
        (Some(e), _) => Err(e.into()),
        (None, r) => r.map_err(CycleError::Store),
    }
}

/// Most shas per `blob_release` (the relay's cap).
const RELEASE_BATCH: usize = 1000;

/// Tell the relay which blobs this device superseded (relay-side GC). A relay
/// without `blob_release` answers with an error: the releases are dropped
/// (the blobs simply stay, as before).
async fn release_blobs(ctx: &Ctx, conn: &Connection) -> Result<(), CycleError> {
    loop {
        let shas = ctx.store.releasable_blobs(RELEASE_BATCH).await.map_err(CycleError::Store)?;
        if shas.is_empty() {
            return Ok(());
        }
        match conn.blob_release(&shas).await {
            Ok(n) => tracing::debug!(event = "sync_blobs_released", released = n),
            Err(TransportError::Relay(code)) => tracing::debug!(event = "sync_blob_release_refused", code = %code),
            Err(e) => return Err(e.into()),
        }
        ctx.store.blobs_released(&shas).await.map_err(CycleError::Store)?;
        if shas.len() < RELEASE_BATCH {
            return Ok(());
        }
    }
}

/// One push: sealed items with their outbox seqs, plus the ops too large to
/// ever send.
#[derive(Default)]
struct PushBatch {
    items: Vec<(i64, WireItem)>,
    oversized: Vec<i64>,
}

/// Seal the largest prefix of `ops` that fits one push (≥ 1 op when any is
/// sendable); oversized ops on the way are set aside for parking.
fn build_push(keys: &SyncKeys, ops: &[(i64, Op)]) -> Result<PushBatch, CycleError> {
    let mut batch = PushBatch::default();
    let mut bytes = 0usize;
    for (seq, op) in ops {
        let (key, blob) = keys
            .seal_op(op)
            .map_err(|e| CycleError::Store(format!("cannot seal op: {e}")))?;
        if blob.len() > MAX_OP_BLOB_CHARS {
            batch.oversized.push(*seq);
            continue;
        }
        let size = blob.len() + key.len() + 32;
        if !batch.items.is_empty() && bytes + size > PUSH_BATCH_BYTES {
            break;
        }
        bytes += size;
        let item = WireItem { key: Some(key), blob, ..Default::default() };
        batch.items.push((*seq, item));
    }
    Ok(batch)
}

/// Relay refusals that are about the op itself: retrying never helps.
fn is_op_rejection(code: &str) -> bool {
    matches!(code, "too_large" | "bad_request")
}

async fn park(ctx: &Ctx, seq: i64, reason: &str) -> Result<(), CycleError> {
    tracing::warn!(event = "sync_op_parked", reason = %reason);
    ctx.store.park(seq, reason).await.map_err(CycleError::Store)
}

/// Push every pending op. Returns the relay's `last_seq` after the last
/// accepted push (`None`: nothing went out).
async fn push_all(ctx: &Ctx, conn: &Connection) -> Result<Option<u64>, CycleError> {
    let mut acked: Option<u64> = None;
    // Everything up to the cursor was applied before these ops were read.
    let base = ctx.store.cursor().await.map_err(CycleError::Store)?;
    loop {
        let ops = ctx.store.pending(PUSH_BATCH).await.map_err(CycleError::Store)?;
        ctx.shared.update(|s| s.pending = ops.len());
        if ops.is_empty() {
            break;
        }
        let batch = build_push(&ctx.keys, &ops)?;
        for seq in &batch.oversized {
            park(ctx, *seq, "too_large").await?;
        }
        if batch.items.is_empty() {
            continue;
        }
        let up_to = batch.items.last().map_or(0, |(seq, _)| *seq);
        let n = batch.items.len();
        let (seqs, items): (Vec<i64>, Vec<WireItem>) = batch.items.into_iter().unzip();
        match conn.push(items.clone(), base).await {
            Ok(ack) => {
                ctx.store.mark_pushed(up_to).await.map_err(CycleError::Store)?;
                ctx.store.note_relay_seq(ack).await.map_err(CycleError::Store)?;
                acked = Some(acked.unwrap_or(0).max(ack));
            }
            // One op spoils the whole (all-or-nothing) batch: send them one by
            // one so only the culprit is parked.
            Err(TransportError::Relay(code)) if is_op_rejection(&code) => {
                for (seq, item) in seqs.into_iter().zip(items) {
                    match conn.push(vec![item], base).await {
                        Ok(ack) => {
                            ctx.store.mark_pushed(seq).await.map_err(CycleError::Store)?;
                            ctx.store.note_relay_seq(ack).await.map_err(CycleError::Store)?;
                            acked = Some(acked.unwrap_or(0).max(ack));
                        }
                        Err(TransportError::Relay(code)) if is_op_rejection(&code) => park(ctx, seq, &code).await?,
                        Err(e) => return Err(e.into()),
                    }
                }
            }
            Err(e) => return Err(e.into()),
        }
        ctx.shared.flush_done.fetch_add(n as u64, Ordering::Relaxed);
        tracing::debug!(event = "sync_pushed", ops = n);
    }
    if acked.is_some() {
        ctx.shared.update(|s| s.last_push = Some(now_ms()));
    }
    Ok(acked)
}

/// Push only the [`PRIORITY_TABLES`] rows (review-3 M1), for when the full
/// push is held back. Oversized ones are left for the full push (it parks
/// them).
async fn push_priority(ctx: &Ctx, conn: &Connection) -> Result<(), CycleError> {
    let ops = ctx.store.pending_in(PRIORITY_TABLES, PUSH_BATCH).await.map_err(CycleError::Store)?;
    if ops.is_empty() {
        return Ok(());
    }
    let batch = build_push(&ctx.keys, &ops)?;
    if batch.items.is_empty() {
        return Ok(());
    }
    let sent: Vec<(i64, Op)> = ops.into_iter().filter(|(seq, _)| batch.items.iter().any(|(s, _)| s == seq)).collect();
    let items = batch.items.into_iter().map(|(_, item)| item).collect();
    let base = ctx.store.cursor().await.map_err(CycleError::Store)?;
    let ack = conn.push(items, base).await?;
    ctx.store.mark_ops_pushed(&sent).await.map_err(CycleError::Store)?;
    ctx.store.note_relay_seq(ack).await.map_err(CycleError::Store)?;
    tracing::debug!(event = "sync_pushed_priority", ops = sent.len());
    Ok(())
}

/// What one [`pull_all`] did.
#[derive(Debug, Default, Clone, Copy)]
struct PullCounts {
    /// Ops of other devices received.
    pulled: usize,
    applied: usize,
    /// Some were `file` / `secret` ops (their deferred ones were retried).
    extra: bool,
}

/// Pull until the relay has no `more`, applying page by page.
async fn pull_all(
    ctx: &Ctx,
    conn: &Connection,
    blobs: &RelayBlobs<'_>,
    progress: &(dyn Fn(RunPhase, Option<String>) + Send + Sync),
) -> Result<PullCounts, CycleError> {
    let mut counts = PullCounts::default();
    let mut cursor = ctx.store.pull_start().await.map_err(CycleError::Store)?;
    let mut undecryptable = 0usize;
    loop {
        progress(RunPhase::Pulling, None);
        let (items, last_seq, more) = conn.pull(cursor, ctx.timing.pull_limit).await?;
        let mut next = cursor;
        let mut ops = Vec::with_capacity(items.len());
        for item in items {
            let seq = item.seq.unwrap_or(0);
            next = next.max(seq);
            if item.device_id.as_deref() == Some(ctx.params.device_id.as_str()) {
                continue; // our own push coming back
            }
            // Tampered, foreign or filed under another row's key: nothing can
            // ever open it, so skip it rather than wedge the cursor.
            match item.key.as_deref().map(|k| ctx.keys.open_op(&item.blob, k)) {
                Some(Ok(op)) => ops.push(op),
                _ => undecryptable += 1,
            }
        }
        counts.pulled += ops.len();
        if !ops.is_empty() {
            progress(RunPhase::Applying, None);
        }
        // Rows first: a file op never depends on them, but a run's row
        // landing before its log keeps the UI consistent.
        let (extra, rows): (Vec<Op>, Vec<Op>) = ops.into_iter().partition(|o| is_extra_tbl(&o.tbl));
        if !rows.is_empty() {
            let report = ctx.store.apply_remote(rows).await.map_err(CycleError::Store)?;
            counts.applied += report.applied;
            tracing::debug!(
                event = "sync_applied",
                applied = report.applied,
                skipped = report.skipped_older,
                conflicts = report.conflicts,
                deferred = report.deferred,
            );
        }
        if !extra.is_empty() {
            counts.extra = true;
            let report = ctx.store.apply_remote_extra(extra, blobs).await.map_err(CycleError::Store)?;
            counts.applied += report.applied;
            tracing::debug!(
                event = "sync_applied_extra",
                applied = report.applied,
                skipped = report.skipped_older,
                conflicts = report.conflicts,
                deferred = report.deferred,
            );
        }
        // The relay's `last_seq` is the last returned seq while `more`, else
        // the group head (superseded ops leave gaps below it).
        next = next.max(last_seq);
        if next != cursor {
            ctx.store.set_cursor(next).await.map_err(CycleError::Store)?;
            ctx.store.note_relay_seq(next).await.map_err(CycleError::Store)?;
            cursor = next;
        }
        if !more {
            break;
        }
    }
    ctx.shared.update(|s| s.last_pull = Some(now_ms()));
    if undecryptable > 0 {
        tracing::warn!(event = "sync_undecryptable_ops", count = undecryptable);
        return Err(CycleError::Store(format!(
            "{undecryptable} op(s) from the relay could not be decrypted and were skipped"
        )));
    }
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::op::OpKind;
    use crate::sync::store::ApplyReport;
    use crate::sync::test_relay::TestRelay;
    use async_trait::async_trait;
    use serde_json::json;

    /// In-memory SyncStore: an outbox and a list of applied ops.
    #[derive(Default)]
    struct FakeStore {
        inner: Mutex<FakeInner>,
    }
    #[derive(Default)]
    struct FakeInner {
        outbox: Vec<(i64, Op)>,
        next_seq: i64,
        applied: Vec<Op>,
        cursor: u64,
        parked: Vec<(String, String)>,
        /// Every `stage` uploads a blob (a live file that keeps growing).
        stage_blobs: bool,
        /// Every `stage` takes this long (a big backlog).
        stage_delay: Option<Duration>,
        /// A selective-sync cursor reset is pending: `set_cursor` is a no-op.
        reset_pending: bool,
        /// Scope of every scoped stage, in order.
        scopes: Vec<StageScope>,
    }

    impl FakeStore {
        fn add(&self, tbl: &str, id: &str, title: &str) {
            let mut g = self.inner.lock().unwrap();
            g.next_seq += 1;
            let seq = g.next_seq;
            g.outbox.push((seq, op(tbl, id, title)));
        }
        fn applied_titles(&self) -> Vec<String> {
            self.inner
                .lock()
                .unwrap()
                .applied
                .iter()
                .map(|o| o.row.as_ref().unwrap()["title"].as_str().unwrap().to_string())
                .collect()
        }
        fn outbox_len(&self) -> usize {
            self.inner.lock().unwrap().outbox.len()
        }
        fn cursor(&self) -> u64 {
            self.inner.lock().unwrap().cursor
        }
        fn parked(&self) -> Vec<(String, String)> {
            self.inner.lock().unwrap().parked.clone()
        }
    }

    #[async_trait]
    impl SyncStore for FakeStore {
        async fn pending(&self, limit: usize) -> Result<Vec<(i64, Op)>, String> {
            Ok(self.inner.lock().unwrap().outbox.iter().take(limit).cloned().collect())
        }
        async fn mark_pushed(&self, up_to: i64) -> Result<(), String> {
            self.inner.lock().unwrap().outbox.retain(|(s, _)| *s > up_to);
            Ok(())
        }
        async fn pending_in(&self, tbls: &[&str], limit: usize) -> Result<Vec<(i64, Op)>, String> {
            let g = self.inner.lock().unwrap();
            Ok(g.outbox.iter().filter(|(_, o)| tbls.contains(&o.tbl.as_str())).take(limit).cloned().collect())
        }
        async fn mark_ops_pushed(&self, ops: &[(i64, Op)]) -> Result<(), String> {
            self.inner.lock().unwrap().outbox.retain(|(s, _)| ops.iter().all(|(p, _)| p != s));
            Ok(())
        }
        async fn apply_remote(&self, ops: Vec<Op>) -> Result<ApplyReport, String> {
            let n = ops.len();
            self.inner.lock().unwrap().applied.extend(ops);
            Ok(ApplyReport {
                applied: n,
                ..Default::default()
            })
        }
        async fn cursor(&self) -> Result<u64, String> {
            Ok(self.inner.lock().unwrap().cursor)
        }
        async fn set_cursor(&self, seq: u64) -> Result<(), String> {
            let mut g = self.inner.lock().unwrap();
            if !g.reset_pending {
                g.cursor = seq;
            }
            Ok(())
        }
        async fn cursor_reset_pending(&self) -> Result<bool, String> {
            Ok(self.inner.lock().unwrap().reset_pending)
        }
        async fn park(&self, seq: i64, reason: &str) -> Result<(), String> {
            let mut g = self.inner.lock().unwrap();
            let pos = g.outbox.iter().position(|(s, _)| *s == seq).ok_or("no such op")?;
            let (_, op) = g.outbox.remove(pos);
            g.parked.push((op.pk.as_str().unwrap_or_default().to_string(), reason.to_string()));
            Ok(())
        }
        async fn parked_count(&self) -> Result<u64, String> {
            Ok(self.inner.lock().unwrap().parked.len() as u64)
        }
        async fn stage_scoped(&self, blobs: &dyn BlobIo, scope: StageScope) -> Result<(), String> {
            self.inner.lock().unwrap().scopes.push(scope);
            self.stage(blobs).await
        }
        async fn stage(&self, blobs: &dyn BlobIo) -> Result<(), String> {
            let delay = self.inner.lock().unwrap().stage_delay;
            if let Some(d) = delay {
                tokio::time::sleep(d).await;
            }
            if self.inner.lock().unwrap().stage_blobs {
                blobs.put(b"growing tail").await?;
            }
            Ok(())
        }
    }

    fn op(tbl: &str, id: &str, title: &str) -> Op {
        Op {
            v: 1,
            schema: 55,
            hlc: serde_json::from_value(json!("0192a3b4c5d60001-dev")).unwrap(),
            tbl: tbl.into(),
            pk: json!(id),
            kind: OpKind::Upsert,
            row: json!({ "id": id, "title": title }).as_object().cloned(),
        }
    }

    fn fast() -> Timing {
        Timing::fast()
    }

    fn keys() -> SyncKeys {
        SyncKeys::derive(&[42u8; 32]).unwrap()
    }

    fn params(relay: &TestRelay, device: &str) -> ConnectParams {
        ConnectParams {
            relay_url: relay.url(),
            auth_token: TestRelay::TOKEN.into(),
            group_id: "g1".into(),
            device_id: device.into(),
            device_token: format!("tok-{device}"),
        }
    }

    struct Device {
        store: Arc<FakeStore>,
        engine: SyncEngine,
        statuses: Arc<Mutex<Vec<SyncStatus>>>,
        handlers: PeerHandlers,
    }

    impl Device {
        /// "Sync from `source`" on this device.
        async fn sync_from(&self, source: &str) -> Result<SyncRunReport, String> {
            self.engine.handle().run_from(source, StageScope::All, &|_, _| {}).await
        }
        fn link(&self) -> Arc<dyn PeerLink> {
            self.engine.handle().link()
        }
    }

    fn device(relay: &TestRelay, id: &str, timing: Timing) -> Device {
        let store = Arc::new(FakeStore::default());
        let statuses = Arc::new(Mutex::new(Vec::new()));
        let sink_statuses = statuses.clone();
        let sink: StatusSink = Arc::new(move |s: &SyncStatus| sink_statuses.lock().unwrap().push(s.clone()));
        let handlers = PeerHandlers::default();
        let engine =
            SyncEngine::start_with(params(relay, id), keys(), store.clone(), sink, false, timing, handlers.clone());
        Device {
            store,
            engine,
            statuses,
            handlers,
        }
    }

    async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
        for _ in 0..200 {
            if f() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("timed out waiting for {what}");
    }

    async fn online(devices: &[&Device]) {
        eventually("online", || devices.iter().all(|d| d.engine.status().state == SyncState::Online)).await;
    }

    #[tokio::test]
    async fn nothing_syncs_on_its_own() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let a = device(&relay, "dev-a", fast());
        let b = device(&relay, "dev-b", fast());
        online(&[&a, &b]).await;
        a.store.add("notes", "n1", "queued");
        a.engine.kick();
        relay.inject("g1", "dev-x", "not pulled");
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(relay.push_attempts("dev-a"), 0, "no push without a request");
        assert!(relay.pulls_of("dev-b").is_empty(), "no pull without a sync");
        assert_eq!(a.store.outbox_len(), 1);
        a.engine.stop().await;
        b.engine.stop().await;
    }

    #[tokio::test]
    async fn sync_run_pulls_what_the_source_flushed() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let a = device(&relay, "dev-a", fast());
        let b = device(&relay, "dev-b", fast());
        online(&[&a, &b]).await;
        assert_eq!(b.engine.handle().online_peers().await.unwrap(), vec!["dev-a"]);

        a.store.add("notes", "n1", "first");
        a.store.add("notes", "n2", "second");
        b.store.add("notes", "n3", "b's own");
        let phases = Arc::new(Mutex::new(Vec::new()));
        let seen = phases.clone();
        let report = b
            .engine
            .handle()
            .run_from("dev-a", StageScope::All, &move |p, _| seen.lock().unwrap().push(p))
            .await
            .unwrap();
        assert_eq!(report, SyncRunReport { pulled: 2, applied: 2, last_seq: 2 });
        assert_eq!(a.store.inner.lock().unwrap().scopes, vec![StageScope::All], "the requester's scope reaches the source");
        assert_eq!(b.store.applied_titles(), vec!["first", "second"]);
        assert_eq!(b.store.cursor(), 2);
        assert_eq!(a.store.outbox_len(), 0);
        assert!(a.engine.status().last_push.is_some());
        assert!(b.engine.status().last_pull.is_some());
        // The receiver pushes nothing of its own; the source applies nothing.
        assert_eq!(b.store.outbox_len(), 1);
        assert_eq!(relay.push_attempts("dev-b"), 0);
        assert!(a.store.applied_titles().is_empty());
        let phases = phases.lock().unwrap().clone();
        assert_eq!(phases.first(), Some(&RunPhase::Requesting));
        assert!(phases.contains(&RunPhase::Pulling) && phases.contains(&RunPhase::Applying), "{phases:?}");
        // The relay only ever saw ciphertext.
        assert!(relay.stored_blobs().iter().all(|b| !b.contains("first")));

        // Nothing new: a second sync is a no-op.
        let again = b.sync_from("dev-a").await.unwrap();
        assert_eq!((again.pulled, again.applied), (0, 0));
        a.engine.stop().await;
        b.engine.stop().await;
    }

    /// The first-join merge runs without an engine: it asks the online
    /// devices to push their outbox, so the relay holds their state.
    #[tokio::test]
    async fn a_merge_connection_flushes_the_online_devices() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let a = device(&relay, "dev-a", fast());
        online(&[&a]).await;
        a.store.add("notes", "n1", "first");
        a.store.add("notes", "n2", "second");

        let (tx, mut rx) = mpsc::unbounded_channel();
        let conn = Connection::connect(&params(&relay, "dev-b"), false, tx).await.unwrap();
        let flushed = flush_online_peers(&conn, &mut rx, &keys(), StageScope::NoSessions, Duration::from_secs(10)).await.unwrap();
        assert_eq!(flushed, vec!["dev-a".to_string()]);
        assert_eq!(a.store.outbox_len(), 0);
        assert_eq!(a.store.inner.lock().unwrap().scopes, vec![StageScope::NoSessions], "no session files for a merge");
        let (items, _, _) = conn.pull(0, 100).await.unwrap();
        assert_eq!(items.len(), 2);

        // A paused device is skipped, not an error.
        a.engine.set_paused(true);
        a.store.add("notes", "n3", "third");
        let flushed = flush_online_peers(&conn, &mut rx, &keys(), StageScope::NoSessions, Duration::from_secs(10)).await.unwrap();
        assert!(flushed.is_empty());
        assert_eq!(a.store.outbox_len(), 1);
        a.engine.stop().await;
    }

    #[tokio::test]
    async fn sync_from_an_offline_or_paused_device_fails_with_its_code() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let a = device(&relay, "dev-a", fast());
        let b = device(&relay, "dev-b", fast());
        online(&[&a, &b]).await;
        assert_eq!(b.sync_from("dev-z").await, Err(PEER_OFFLINE.to_string()));
        assert!(b.sync_from("dev-b").await.is_err(), "not from itself");

        a.store.add("notes", "n1", "while paused");
        a.engine.set_paused(true);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(a.engine.status().state, SyncState::Online, "a paused device stays connected");
        assert_eq!(b.sync_from("dev-a").await, Err(PEER_PAUSED.to_string()));
        assert_eq!(a.store.outbox_len(), 1, "a paused device serves nothing");
        a.engine.set_paused(false);
        b.sync_from("dev-a").await.unwrap();
        assert_eq!(b.store.applied_titles(), vec!["while paused"]);
        assert!(!a.engine.status().paused);

        a.engine.stop().await;
        for _ in 0..200 {
            if b.engine.handle().online_peers().await.unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(b.sync_from("dev-a").await, Err(PEER_OFFLINE.to_string()));
        b.engine.stop().await;
    }

    #[tokio::test]
    async fn later_op_supersedes_earlier_on_the_relay() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let a = device(&relay, "dev-a", fast());
        online(&[&a]).await;
        a.store.add("notes", "n1", "v1");
        a.engine.handle().flush_outbox(StageScope::All).await.unwrap();
        assert_eq!(relay.op_count(), 1);
        a.store.add("notes", "n1", "v2");
        assert_eq!(a.engine.handle().flush_outbox(StageScope::All).await.unwrap(), 2);
        assert_eq!(a.store.outbox_len(), 0);
        assert_eq!(relay.op_count(), 1, "same key replaces the older op");

        let b = device(&relay, "dev-b", fast());
        online(&[&b]).await;
        b.sync_from("dev-a").await.unwrap();
        assert_eq!(b.store.applied_titles(), vec!["v2"]);
        assert_eq!(b.store.cursor(), 2);
        a.engine.stop().await;
        b.engine.stop().await;
    }

    #[tokio::test]
    async fn pull_paginates_until_not_more() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let mut t = fast();
        t.pull_limit = 3;
        let a = device(&relay, "dev-a", fast());
        let b = device(&relay, "dev-b", t);
        online(&[&a, &b]).await;
        for i in 0..10 {
            a.store.add("todos", &format!("t{i}"), &format!("todo {i}"));
        }
        let report = b.sync_from("dev-a").await.unwrap();
        assert_eq!((report.pulled, report.last_seq), (10, 10));
        assert_eq!(b.store.applied_titles().len(), 10);
        assert_eq!(b.store.cursor(), 10);
        let pulls = relay.pulls_of("dev-b");
        assert!(pulls.iter().all(|l| *l == 3), "{pulls:?}");
        assert!(pulls.len() >= 4, "10 ops in pages of 3: {pulls:?}");
        a.engine.stop().await;
        b.engine.stop().await;
    }

    #[tokio::test]
    async fn reconnects_after_the_relay_drops_the_socket() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let a = device(&relay, "dev-a", fast());
        let b = device(&relay, "dev-b", fast());
        online(&[&a, &b]).await;
        let hellos = relay.hello_count();
        relay.drop_all();
        eventually("reconnected", || relay.hello_count() >= hellos + 2).await;
        online(&[&a, &b]).await;
        a.store.add("notes", "n1", "after drop");
        b.sync_from("dev-a").await.unwrap();
        assert_eq!(b.store.applied_titles(), vec!["after drop"]);
        // The drop was visible as offline at some point.
        assert!(a.statuses.lock().unwrap().iter().any(|s| s.state == SyncState::Offline));
        a.engine.stop().await;
        b.engine.stop().await;
    }

    #[tokio::test]
    async fn revoked_device_reports_error_and_stops_retrying() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        relay.revoke("g1", "dev-a");
        let a = device(&relay, "dev-a", fast());
        eventually("error", || a.engine.status().state == SyncState::Error).await;
        let err = a.engine.status().last_error.unwrap();
        assert!(err.contains("device_revoked"), "{err}");
        let hellos = relay.hello_count();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(relay.hello_count(), hellos, "no retry storm after a rejection");
        a.engine.stop().await;
    }

    #[tokio::test]
    async fn undecryptable_ops_are_skipped_and_reported() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        relay.inject("g1", "dev-x", "garbage-not-a-sealed-op");
        let a = device(&relay, "dev-a", fast());
        let b = device(&relay, "dev-b", fast());
        online(&[&a, &b]).await;
        let err = b.sync_from("dev-a").await.unwrap_err();
        assert!(err.contains("could not be decrypted"), "{err}");
        assert_eq!(b.store.cursor(), 1, "cursor moves past the bad op");
        a.engine.stop().await;
        b.engine.stop().await;
    }

    /// Echoes its body back in two replies.
    struct Echo;

    #[async_trait]
    impl PeerHandler for Echo {
        fn kinds(&self) -> &'static [&'static str] {
            &["echo"]
        }
        async fn handle(&self, _link: Arc<dyn PeerLink>, from: String, _kind: String, body: Value, reply: Arc<dyn Reply>) {
            let _ = reply.send("echo_part", json!({ "from": from }), false).await;
            let _ = reply.send("echo_done", body, true).await;
        }
    }

    /// Holds its request without ever answering.
    struct Silent(Mutex<Vec<Arc<dyn Reply>>>);

    #[async_trait]
    impl PeerHandler for Silent {
        fn kinds(&self) -> &'static [&'static str] {
            &["silent"]
        }
        async fn handle(&self, _link: Arc<dyn PeerLink>, _from: String, _kind: String, _body: Value, reply: Arc<dyn Reply>) {
            self.0.lock().unwrap().push(reply);
        }
    }

    async fn collect(mut r: Replies) -> Vec<Result<(String, Value), String>> {
        let mut out = Vec::new();
        while let Some(item) = tokio::time::timeout(Duration::from_secs(5), r.recv()).await.expect("stream ends") {
            out.push(item);
        }
        out
    }

    #[tokio::test]
    async fn peer_requests_round_trip_between_two_devices() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let mut t = fast();
        t.peer_timeout = Duration::from_millis(300);
        let a = device(&relay, "dev-a", t);
        let b = device(&relay, "dev-b", fast());
        online(&[&a, &b]).await;
        // Registered after the engine started.
        b.handlers.write().unwrap().push(Arc::new(Echo));
        b.handlers.write().unwrap().push(Arc::new(Silent(Mutex::new(Vec::new()))));

        let link = a.link();
        assert_eq!(link.online_peers().await.unwrap(), vec!["dev-b"]);
        let replies = link.request("dev-b", "echo", json!({ "n": 7 })).await.unwrap();
        assert_eq!(
            collect(replies).await,
            vec![Ok(("echo_part".into(), json!({ "from": "dev-a" }))), Ok(("echo_done".into(), json!({ "n": 7 })))]
        );
        let unknown = collect(link.request("dev-b", "nope", json!({})).await.unwrap()).await;
        assert_eq!(unknown, vec![Err("unknown request kind nope".to_string())]);
        assert_eq!(link.request("dev-z", "echo", json!({})).await.err(), Some(PEER_OFFLINE.to_string()));
        // No reply within the timeout ends the stream with an error.
        let silent = collect(link.request("dev-b", "silent", json!({})).await.unwrap()).await;
        assert_eq!(silent, vec![Err(PEER_TIMEOUT.to_string())]);
        assert!(a.engine.handle().ctx.shared.pending.lock().unwrap().is_empty(), "nothing left waiting");
        a.engine.stop().await;
        b.engine.stop().await;
    }

    #[tokio::test]
    async fn requests_in_flight_fail_when_the_connection_drops() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let a = device(&relay, "dev-a", fast());
        let b = device(&relay, "dev-b", fast());
        online(&[&a, &b]).await;
        b.handlers.write().unwrap().push(Arc::new(Silent(Mutex::new(Vec::new()))));
        let replies = a.link().request("dev-b", "silent", json!({})).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        relay.drop_all();
        let got = collect(replies).await;
        assert_eq!(got.len(), 1);
        assert!(got[0].is_err());
        a.engine.stop().await;
        b.engine.stop().await;
    }

    #[tokio::test]
    async fn link_blobs_are_sealed_and_released() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let a = device(&relay, "dev-a", fast());
        let b = device(&relay, "dev-b", fast());
        online(&[&a, &b]).await;
        let sha = a.link().blob_put(b"file contents".to_vec()).await.unwrap();
        assert!(relay.stored_blobs().iter().all(|b| !b.contains("file contents")));
        assert_eq!(b.link().blob_get(&sha).await.unwrap(), b"file contents");
        assert_eq!(b.link().blob_get("00").await.err(), Some("blob_not_found".to_string()));
        b.link().blob_release(std::slice::from_ref(&sha)).await.unwrap();
        assert_eq!(relay.blob_counts(), (0, 1));
        let too_big = vec![0u8; MAX_BLOB_PLAINTEXT + 1];
        assert_eq!(a.link().blob_put(too_big).await.err(), Some("too_large".to_string()));

        // A refused upload: the next ones fail fast for a while.
        relay.fail_blob_puts_of("dev-a", Some("quota_exceeded"));
        assert_eq!(a.link().blob_put(b"x".to_vec()).await.err(), Some("quota_exceeded".to_string()));
        let tries = relay.blob_put_attempts("dev-a");
        assert_eq!(a.link().blob_put(b"y".to_vec()).await.err(), Some("quota_exceeded".to_string()));
        assert_eq!(relay.blob_put_attempts("dev-a"), tries, "no upload during the backoff");
        relay.fail_blob_puts_of("dev-a", None);
        tokio::time::sleep(Duration::from_millis(100)).await;
        a.link().blob_put(b"z".to_vec()).await.unwrap();
        a.engine.stop().await;
        b.engine.stop().await;
    }

    /// Real stores on both sides: a run log and a Keychain item created on A
    /// reach B through the relay (blobs sealed, ops routed to files/secrets)
    /// when B syncs from A.
    #[tokio::test]
    async fn files_and_secrets_route_through_the_relay() {
        use crate::sync::codec::PathCtx;
        use crate::sync::files::tests::{mem_db, tempdir};
        use crate::sync::secrets::tests::FakeKeychain;
        use crate::sync::store::SqliteSyncStore;

        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let timing = crate::sync::files::Timing { quick: Duration::from_millis(50), full: Duration::from_millis(200) };
        let mut sides = Vec::new();
        for id in ["dev-a", "dev-b"] {
            let home = tempdir();
            let db = mem_db().await;
            let kc = Arc::new(FakeKeychain::default());
            let paths = PathCtx::with_home(home.path()).with_app_data(home.path().join("data"));
            let store = SqliteSyncStore::new(db.clone(), id.into())
                .with_paths(paths)
                .with_secrets(kc.clone())
                .with_file_timing(timing.clone());
            let sink: StatusSink = Arc::new(|_: &SyncStatus| {});
            let engine = SyncEngine::start(params(&relay, id), keys(), Arc::new(store), sink, false, fast());
            sides.push((home, db, kc, engine));
        }
        let (ha, db_a, kc_a, _) = &sides[0];
        let (hb, db_b, kc_b, engine_b) = &sides[1];
        let b = engine_b.handle();
        eventually("online", || sides.iter().all(|s| s.3.status().state == SyncState::Online)).await;
        // Sync from A until `done` holds (a live file's tail is held back a
        // little while; full scans come every 200 ms).
        let sync_until = |what: &'static str, done: Box<dyn Fn() -> bool>| {
            let b = b.clone();
            async move {
                for _ in 0..100 {
                    b.run_from("dev-a", StageScope::All, &|_, _| {}).await.unwrap();
                    if done() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                panic!("timed out waiting for {what}");
            }
        };

        let project = ha.path().join("w/p");
        std::fs::create_dir_all(project.join(".devdy/runs")).unwrap();
        let log = project.join(".devdy/runs/r1.log");
        std::fs::write(&log, "line 1\n").unwrap();
        sqlx::query("INSERT INTO projects (id, name, path, created_at) VALUES ('p1', 'p', ?, 'x')")
            .bind(project.to_string_lossy().as_ref())
            .execute(db_a)
            .await
            .unwrap();
        sqlx::query("INSERT INTO runs (id, project_id, type, status, output_path, created_at) VALUES ('r1', 'p1', 'session', 'running', ?, 'x')")
            .bind(log.to_string_lossy().as_ref())
            .execute(db_a)
            .await
            .unwrap();
        kc_a.put("github", "acc1", "ghp_from_a");

        let log_b = hb.path().join("w/p/.devdy/runs/r1.log");
        let read_b = {
            let log_b = log_b.clone();
            move || std::fs::read_to_string(&log_b).ok()
        };
        let rb = read_b.clone();
        sync_until("log on b", Box::new(move || rb().as_deref() == Some("line 1\n"))).await;
        let kcb = kc_b.clone();
        sync_until("secret on b", Box::new(move || kcb.get("github", "acc1").as_deref() == Some("ghp_from_a"))).await;
        let out_b: String = sqlx::query_scalar("SELECT output_path FROM runs WHERE id = 'r1'").fetch_one(db_b).await.unwrap();
        assert_eq!(out_b, log_b.to_string_lossy(), "the run row points at the synced file");

        // The next sync brings the appended lines.
        std::fs::write(&log, "line 1\nline 2\n").unwrap();
        let rb = read_b.clone();
        sync_until("append on b", Box::new(move || rb().as_deref() == Some("line 1\nline 2\n"))).await;
        // A's superseded tail chunk is released on the relay once the new
        // manifest is pushed.
        let r = relay.clone();
        sync_until("old tail released", Box::new(move || r.blob_counts().1 >= 1)).await;
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_blob_release").fetch_one(db_a).await.unwrap();
        assert_eq!(left, 0);
        // Nothing but sealed bytes on the relay.
        assert!(relay.stored_blobs().iter().all(|b| !b.contains("ghp_from_a")));
        // B does not echo what it applied, even when it flushes itself.
        b.flush_outbox(StageScope::All).await.unwrap();
        let echoed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_outbox WHERE tbl IN ('file', 'secret')").fetch_one(db_b).await.unwrap();
        assert_eq!(echoed, 0);

        for (_, _, _, engine) in sides {
            engine.stop().await;
        }
    }

    #[test]
    fn push_batches_respect_the_byte_cap() {
        let k = keys();
        let big = "x".repeat(PUSH_BATCH_BYTES / 3);
        let ops: Vec<(i64, Op)> = (1..=5).map(|i| (i, op("notes", &format!("n{i}"), &big))).collect();
        let b = build_push(&k, &ops).unwrap();
        assert_eq!(b.items.iter().map(|i| i.0).collect::<Vec<_>>(), vec![1, 2]);
        assert!(b.oversized.is_empty());
        // An op over the batch budget but under the relay's cap goes out alone.
        let alone = vec![
            (8, op("notes", "n8", &"y".repeat(PUSH_BATCH_BYTES * 3 / 4 + 1000))),
            (9, op("notes", "n9", "small")),
        ];
        assert_eq!(build_push(&k, &alone).unwrap().items.iter().map(|i| i.0).collect::<Vec<_>>(), vec![8]);
        // One the relay would refuse is set aside, the rest still batches.
        let huge = vec![
            (9, op("notes", "n9", &"y".repeat(MAX_OP_BLOB_CHARS))),
            (10, op("notes", "n10", "small")),
        ];
        let b = build_push(&k, &huge).unwrap();
        assert_eq!(b.oversized, vec![9]);
        assert_eq!(b.items.iter().map(|i| i.0).collect::<Vec<_>>(), vec![10]);
    }

    #[test]
    fn push_backoff_doubles_up_to_the_cap() {
        let mut b = PushBackoff::default();
        assert!(b.due());
        let (min, max) = (Duration::from_secs(1), Duration::from_secs(5));
        let waits: Vec<u64> = (0..5).map(|_| b.failed(min, max).as_secs()).collect();
        assert_eq!(waits, vec![1, 2, 4, 5, 5]);
        assert!(!b.due());
    }

    /// Finding #4: an op the relay refuses for good is parked; the rest of
    /// the queue goes out, and pulls never stop.
    #[tokio::test]
    async fn refused_op_is_parked_and_sync_goes_on() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        relay.reject_blobs_over(2000);
        let a = device(&relay, "dev-a", fast());
        let b = device(&relay, "dev-b", fast());
        online(&[&a, &b]).await;
        b.store.add("notes", "n1", "small 1");
        b.store.add("notes", "big", &"x".repeat(4000));
        b.store.add("notes", "n2", "small 2");
        a.sync_from("dev-b").await.unwrap();
        assert_eq!(b.store.outbox_len(), 0);
        assert_eq!(b.store.parked(), vec![("big".to_string(), "too_large".to_string())]);
        assert_eq!(a.store.applied_titles(), vec!["small 1", "small 2"]);
        assert_eq!(b.engine.status().parked, 1);
        // B still pulls.
        a.store.add("notes", "n3", "from a");
        b.sync_from("dev-a").await.unwrap();
        assert_eq!(b.store.applied_titles(), vec!["from a"]);
        a.engine.stop().await;
        b.engine.stop().await;
    }

    /// Finding #4: a push refused for a non-op reason (quota) fails the
    /// sync with the relay's code; pulls go on, and the next sync after the
    /// relay recovers delivers.
    #[tokio::test]
    async fn refused_push_fails_the_sync_until_the_relay_takes_it() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let a = device(&relay, "dev-a", fast());
        let b = device(&relay, "dev-b", fast());
        online(&[&a, &b]).await;
        relay.fail_pushes_of("dev-b", Some("quota_exceeded"));
        b.store.add("notes", "n1", "stuck");
        assert_eq!(a.sync_from("dev-b").await, Err("quota_exceeded".to_string()));
        assert!(b.engine.status().last_error.is_some_and(|e| e.contains("quota_exceeded")));
        a.store.add("notes", "n2", "from a");
        b.sync_from("dev-a").await.unwrap();
        assert_eq!(b.store.applied_titles(), vec!["from a"]);
        relay.fail_pushes_of("dev-b", None);
        a.sync_from("dev-b").await.unwrap();
        assert_eq!(b.store.outbox_len(), 0);
        assert_eq!(a.store.applied_titles(), vec!["stuck"]);
        assert_eq!(b.engine.status().last_error, None);
        a.engine.stop().await;
        b.engine.stop().await;
    }

    /// Findings N11 / review-3 M1: when the relay refuses the blobs staging
    /// uploads (quota), nothing else is pushed but run ownership / lease
    /// rows, and the sync fails with the relay's code.
    #[tokio::test]
    async fn refused_blob_upload_holds_the_push_but_not_lease_renewals() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let a = device(&relay, "dev-a", fast());
        let b = device(&relay, "dev-b", fast());
        online(&[&a, &b]).await;
        relay.fail_blob_puts_of("dev-b", Some("quota_exceeded"));
        b.store.inner.lock().unwrap().stage_blobs = true;
        b.store.add("notes", "n1", "held back");
        b.store.add("runs", "r1", "lease 1");
        assert_eq!(a.sync_from("dev-b").await, Err("quota_exceeded".to_string()));
        assert_eq!(relay.push_attempts("dev-b"), 1, "only the lease went out");
        assert_eq!(relay.op_count(), 1);
        assert_eq!(b.store.outbox_len(), 1, "the rest stays queued");
        relay.fail_blob_puts_of("dev-b", None);
        a.sync_from("dev-b").await.unwrap();
        assert_eq!(b.store.outbox_len(), 0);
        assert_eq!(a.store.applied_titles(), vec!["lease 1", "held back"]);
        a.engine.stop().await;
        b.engine.stop().await;
    }

    /// The SQLite side of the priority push: only the listed tables' rows are
    /// read and marked, everything else stays queued in order.
    #[tokio::test]
    async fn sqlite_store_pushes_priority_rows_alone() {
        use crate::sync::files::tests::mem_db;
        use crate::sync::store::SqliteSyncStore;
        let db = mem_db().await;
        let store = SqliteSyncStore::new(db.clone(), "dev-a".into());
        for sql in [
            "INSERT INTO todos (id, text, position, created_at) VALUES ('t1', 'x', 0, 'x')",
            "INSERT INTO projects (id, name, path, created_at) VALUES ('p1', 'P', '/nonexistent/p1', 'x')",
            "INSERT INTO runs (id, project_id, type, status, engine, created_at) VALUES ('r1', 'p1', 'session', 'running', 'claude', 'x')",
        ] {
            sqlx::query(sql).execute(&db).await.unwrap();
        }
        let prio = store.pending_in(PRIORITY_TABLES, 10).await.unwrap();
        assert_eq!(prio.iter().map(|(_, o)| o.tbl.as_str()).collect::<Vec<_>>(), vec!["runs"]);
        store.mark_ops_pushed(&prio).await.unwrap();
        let left: Vec<String> = store.pending(10).await.unwrap().into_iter().map(|(_, o)| o.tbl).collect();
        assert_eq!(left, vec!["todos", "projects"]);
        assert!(store.pending_in(PRIORITY_TABLES, 10).await.unwrap().is_empty());
        assert!(store.pending_in(&["file"], 10).await.is_err());
    }

    #[tokio::test]
    async fn sqlite_store_parks_until_the_row_changes_again() {
        use crate::sync::files::tests::mem_db;
        use crate::sync::store::SqliteSyncStore;
        let db = mem_db().await;
        let store = SqliteSyncStore::new(db.clone(), "dev-a".into());
        let exec = |sql: &'static str| {
            let db = db.clone();
            async move { sqlx::query(sql).execute(&db).await.unwrap() }
        };
        exec("INSERT INTO todos (id, text, position, created_at) VALUES ('t1', 'huge', 0, 'x'), ('t2', 'ok', 1, 'x')").await;
        let pending = store.pending(10).await.unwrap();
        let seq = pending.iter().find(|(_, o)| o.pk == serde_json::json!("t1")).unwrap().0;
        store.park(seq, "too_large").await.unwrap();
        assert_eq!(store.parked_count().await.unwrap(), 1);
        let left: Vec<_> = store.pending(10).await.unwrap().into_iter().map(|(_, o)| o.pk).collect();
        assert_eq!(left, vec![serde_json::json!("t2")]);
        // A newer change is queued and, once pushed, clears the record.
        exec("UPDATE todos SET text = 'small' WHERE id = 't1'").await;
        let up_to = store.pending(10).await.unwrap().iter().map(|(s, _)| *s).max().unwrap();
        assert_eq!(store.parked_count().await.unwrap(), 1);
        store.mark_pushed(up_to).await.unwrap();
        assert_eq!(store.parked_count().await.unwrap(), 0);
    }

    /// Review #3: a flush that takes longer than the requester's inactivity
    /// timeout keeps it alive with progress replies.
    #[tokio::test]
    async fn a_long_flush_sends_progress_and_does_not_time_out() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let mut src = fast();
        src.peer_keepalive = Duration::from_millis(250);
        let mut dst = fast();
        dst.peer_timeout = Duration::from_millis(1500);
        let a = device(&relay, "dev-a", src);
        let b = device(&relay, "dev-b", dst);
        online(&[&a, &b]).await;
        a.store.inner.lock().unwrap().stage_delay = Some(Duration::from_millis(4000));
        a.store.add("notes", "n1", "slow");
        let phases = Arc::new(Mutex::new(Vec::new()));
        let seen = phases.clone();
        let report = b
            .engine
            .handle()
            .run_from("dev-a", StageScope::All, &move |p, m| seen.lock().unwrap().push((p, m)))
            .await
            .unwrap();
        assert_eq!(report.applied, 1);
        let phases = phases.lock().unwrap().clone();
        assert!(
            phases.iter().any(|(p, m)| *p == RunPhase::Flushing && m.as_deref() == Some("0")),
            "{phases:?}"
        );
        a.engine.stop().await;
        b.engine.stop().await;
    }

    /// Waits until released.
    struct Hold(Arc<Notify>);

    #[async_trait]
    impl PeerHandler for Hold {
        fn kinds(&self) -> &'static [&'static str] {
            &["hold"]
        }
        async fn handle(&self, _link: Arc<dyn PeerLink>, _from: String, _kind: String, _body: Value, reply: Arc<dyn Reply>) {
            self.0.notified().await;
            let _ = reply.send("held", json!({}), true).await;
        }
    }

    #[tokio::test]
    async fn requests_of_one_device_beyond_the_cap_are_busy() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let a = device(&relay, "dev-a", fast());
        let b = device(&relay, "dev-b", fast());
        online(&[&a, &b]).await;
        let release = Arc::new(Notify::new());
        b.handlers.write().unwrap().push(Arc::new(Hold(release.clone())));
        let mut held = Vec::new();
        for _ in 0..MAX_SERVED_PER_PEER {
            held.push(a.link().request("dev-b", "hold", json!({})).await.unwrap());
        }
        eventually("all held", || {
            b.engine.handle().ctx.shared.serving.lock().unwrap().get("dev-a") == Some(&MAX_SERVED_PER_PEER)
        })
        .await;
        let extra = collect(a.link().request("dev-b", "hold", json!({})).await.unwrap()).await;
        assert_eq!(extra, vec![Err(PEER_BUSY.to_string())]);
        release.notify_waiters();
        for r in held {
            assert_eq!(collect(r).await, vec![Ok(("held".to_string(), json!({})))]);
        }
        eventually("slots freed", || b.engine.handle().ctx.shared.serving.lock().unwrap().is_empty()).await;
        a.engine.stop().await;
        b.engine.stop().await;
    }

    /// Review #5: the old leader tells the new one, which syncs from it at
    /// once (and confirms); an offline new leader is reported.
    #[tokio::test]
    async fn the_new_leader_syncs_from_the_old_one_when_told() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let a = device(&relay, "dev-a", fast());
        let b = device(&relay, "dev-b", fast());
        online(&[&a, &b]).await;
        a.store.add("settings", "sync_leader_device", "dev-b");
        a.engine.handle().notify_leader("dev-b").await.unwrap();
        assert_eq!(b.store.applied_titles(), vec!["dev-b"]);
        assert_eq!(a.engine.handle().notify_leader("dev-z").await, Err(PEER_OFFLINE.to_string()));
        a.engine.stop().await;
        b.engine.stop().await;
    }

    /// Review #8: a failed flush leaves `pending` at what is still queued.
    #[tokio::test]
    async fn a_failed_flush_recounts_pending() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let a = device(&relay, "dev-a", fast());
        let b = device(&relay, "dev-b", fast());
        online(&[&a, &b]).await;
        relay.fail_blob_puts_of("dev-b", Some("quota_exceeded"));
        b.store.inner.lock().unwrap().stage_blobs = true;
        b.store.add("notes", "n1", "one");
        b.store.add("notes", "n2", "two");
        assert!(a.sync_from("dev-b").await.is_err());
        assert_eq!(b.engine.status().pending, 2);
        relay.fail_blob_puts_of("dev-b", None);
        a.sync_from("dev-b").await.unwrap();
        assert_eq!(b.engine.status().pending, 0);
        a.engine.stop().await;
        b.engine.stop().await;
    }

    /// Review #4: "Continue here" syncs from the owner first, so a lease the
    /// owner still renews is seen as live (the take-over is refused), and an
    /// unreachable owner is reported instead of read as expired.
    #[tokio::test]
    async fn continue_here_decides_on_the_owners_current_lease() {
        use crate::sync::codec::PathCtx;
        use crate::sync::files::tests::{mem_db, tempdir};
        use crate::sync::sessions;
        use crate::sync::store::SqliteSyncStore;

        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let mut sides = Vec::new();
        for _ in 0..2 {
            let home = tempdir();
            let db = mem_db().await;
            let id = crate::sync::ensure_device(&db).await.unwrap();
            let paths = PathCtx::with_home(home.path()).with_app_data(home.path().join("data"));
            let store = SqliteSyncStore::new(db.clone(), id.clone()).with_paths(paths);
            let sink: StatusSink = Arc::new(|_: &SyncStatus| {});
            let engine = SyncEngine::start(params(&relay, &id), keys(), Arc::new(store), sink, false, fast());
            sides.push((home, db, id, engine));
        }
        eventually("online", || sides.iter().all(|s| s.3.status().state == SyncState::Online)).await;
        let (home_b, db_b, _, engine_b) = sides.pop().unwrap();
        let (home_a, db_a, id_a, engine_a) = sides.pop().unwrap();
        let (db_a, db_b, id_a) = (&db_a, &db_b, &id_a);
        let project = home_a.path().join("p");
        std::fs::create_dir_all(&project).unwrap();
        sqlx::query("INSERT INTO projects (id, name, path, created_at) VALUES ('p1', 'p', ?, 'x')")
            .bind(project.to_string_lossy().as_ref())
            .execute(db_a)
            .await
            .unwrap();
        sqlx::query("INSERT INTO runs (id, project_id, type, status, engine, created_at) VALUES ('r1', 'p1', 'session', 'running', 'claude', 'x')")
            .execute(db_a)
            .await
            .unwrap();
        sessions::claim(db_a, "r1", &project.to_string_lossy()).await.unwrap();
        engine_b.handle().run_from(id_a, StageScope::All, &|_, _| {}).await.unwrap();
        let lease = |db: &crate::db::Db| {
            let db = db.clone();
            async move {
                sqlx::query_scalar::<_, Option<String>>("SELECT lease_until FROM runs WHERE id = 'r1'")
                    .fetch_one(&db)
                    .await
                    .unwrap()
            }
        };
        // A renews while it runs; B only has the lease of its last sync.
        tokio::time::sleep(Duration::from_millis(20)).await;
        sessions::renew(db_a, &["r1".to_string()]).await.unwrap();
        assert_ne!(lease(db_b).await, lease(db_a).await);

        sessions::refresh_from_owner(Some(engine_b.handle()), id_a).await.unwrap();
        assert_eq!(lease(db_b).await, lease(db_a).await, "B decides on A's current lease");
        let err = sessions::take_over(db_b, "r1", false, false, false).await.unwrap_err();
        assert!(!err.is_empty(), "still running on A: refused");

        engine_a.stop().await;
        for _ in 0..200 {
            if engine_b.handle().online_peers().await.unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(
            sessions::refresh_from_owner(Some(engine_b.handle()), id_a).await,
            Err(PEER_OFFLINE.to_string())
        );
        assert_eq!(
            sessions::refresh_from_owner(None, id_a).await,
            Err(NOT_CONNECTED.to_string())
        );
        engine_b.stop().await;
        drop((home_a, home_b));
    }

    /// Review C: selective sync reset the cursor during the pull; the sync
    /// still succeeds (the next one pulls everything again).
    #[tokio::test]
    async fn a_cursor_reset_during_a_sync_is_not_reported_as_relay_behind() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let a = device(&relay, "dev-a", fast());
        let b = device(&relay, "dev-b", fast());
        online(&[&a, &b]).await;
        a.store.add("notes", "n1", "one");
        b.store.inner.lock().unwrap().reset_pending = true;
        let report = b.sync_from("dev-a").await.unwrap();
        assert_eq!((report.applied, report.last_seq), (1, 1));
        assert_eq!(b.store.cursor(), 0, "left for the next sync to re-pull");
        a.engine.stop().await;
        b.engine.stop().await;
    }

    #[tokio::test]
    async fn sqlite_store_reports_a_pending_cursor_reset() {
        use crate::sync::files::tests::mem_db;
        use crate::sync::store::SqliteSyncStore;
        let db = mem_db().await;
        let store = SqliteSyncStore::new(db.clone(), "dev-a".into());
        assert!(!store.cursor_reset_pending().await.unwrap());
        sqlx::query("INSERT INTO sync_state (key, value) VALUES (?, '1')")
            .bind(crate::sync::prefs::CURSOR_RESET_KEY)
            .execute(&db)
            .await
            .unwrap();
        assert!(store.cursor_reset_pending().await.unwrap());
    }

    /// Sets the flag when dropped (the handler's task was aborted).
    struct OnDrop(Arc<std::sync::atomic::AtomicBool>);

    impl Drop for OnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// Never finishes on its own; `chatty` sends a non-last reply every 30 ms.
    struct Endless {
        dropped: Arc<std::sync::atomic::AtomicBool>,
        chatty: bool,
    }

    #[async_trait]
    impl PeerHandler for Endless {
        fn kinds(&self) -> &'static [&'static str] {
            &["endless"]
        }
        async fn handle(&self, _link: Arc<dyn PeerLink>, _from: String, _kind: String, _body: Value, reply: Arc<dyn Reply>) {
            let _guard = OnDrop(self.dropped.clone());
            loop {
                tokio::time::sleep(Duration::from_millis(30)).await;
                if self.chatty {
                    let _ = reply.send("tick", json!({}), false).await;
                }
            }
        }
    }

    fn endless(b: &Device, chatty: bool) -> Arc<std::sync::atomic::AtomicBool> {
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        b.handlers.write().unwrap().push(Arc::new(Endless { dropped: dropped.clone(), chatty }));
        dropped
    }

    fn nothing_served(d: &Device) -> bool {
        let shared = &d.engine.handle().ctx.shared;
        shared.serving.lock().unwrap().is_empty() && shared.served.lock().unwrap().is_empty()
    }

    /// Review D: a request the requester gave up on — replies dropped, or
    /// timed out — is cancelled on the source: its task is aborted and its
    /// slot freed.
    #[tokio::test]
    async fn abandoned_requests_are_cancelled_on_the_source() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let mut t = fast();
        t.peer_timeout = Duration::from_millis(300);
        let a = device(&relay, "dev-a", t);
        let b = device(&relay, "dev-b", fast());
        online(&[&a, &b]).await;
        let dropped = endless(&b, false);

        let replies = a.link().request("dev-b", "endless", json!({})).await.unwrap();
        eventually("served", || !nothing_served(&b)).await;
        drop(replies);
        eventually("aborted after the drop", || dropped.load(Ordering::SeqCst) && nothing_served(&b)).await;

        dropped.store(false, Ordering::SeqCst);
        let got = collect(a.link().request("dev-b", "endless", json!({})).await.unwrap()).await;
        assert_eq!(got, vec![Err(PEER_TIMEOUT.to_string())]);
        eventually("aborted after the timeout", || dropped.load(Ordering::SeqCst) && nothing_served(&b)).await;
        a.engine.stop().await;
        b.engine.stop().await;
    }

    /// Review D: a reply that cannot reach the requester (gone) stops the work.
    #[tokio::test]
    async fn work_for_a_requester_that_is_gone_stops() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let a = device(&relay, "dev-a", fast());
        let b = device(&relay, "dev-b", fast());
        online(&[&a, &b]).await;
        let dropped = endless(&b, true);
        let replies = a.link().request("dev-b", "endless", json!({})).await.unwrap();
        eventually("served", || !nothing_served(&b)).await;
        // The requester disappears without cancelling (crash / quit).
        std::mem::forget(replies);
        a.engine.stop().await;
        eventually("aborted", || dropped.load(Ordering::SeqCst) && nothing_served(&b)).await;
        b.engine.stop().await;
    }

    type Side = (crate::db::Db, Ctx, Connection);

    /// A device on a real SQLite store whose cycle steps are driven by hand.
    async fn manual_side(relay: &TestRelay, id: &str) -> Side {
        use crate::sync::files::tests::mem_db;
        use crate::sync::store::SqliteSyncStore;
        let db = mem_db().await;
        let store: Arc<dyn SyncStore> = Arc::new(SqliteSyncStore::new(db.clone(), id.into()));
        let (tx, _rx) = mpsc::unbounded_channel();
        let conn = Connection::connect(&params(relay, id), false, tx).await.unwrap();
        let ctx = Ctx {
            params: params(relay, id),
            keys: keys(),
            store,
            shared: Arc::new(Shared::new(Arc::new(|_: &SyncStatus| {}), false, PeerHandlers::default())),
            timing: fast(),
        };
        (db, ctx, conn)
    }

    async fn push_side(side: &Side) {
        push_all(&side.1, &side.2).await.unwrap();
    }

    async fn pull_side(side: &Side) {
        let blobs = RelayBlobs::new(&side.2, &side.1.keys);
        pull_all(&side.1, &side.2, &blobs, &|_, _| {}).await.unwrap();
    }

    async fn owner_of(db: &crate::db::Db, run: &str) -> (Option<String>, i64, Option<String>) {
        sqlx::query_as("SELECT owner_device, owner_epoch, lease_until FROM runs WHERE id = ?")
            .bind(run)
            .fetch_one(db)
            .await
            .unwrap()
    }

    /// Finding N4: A owns a run and is cut off; B takes it over (epoch 1),
    /// then A — not knowing — resumes and renews its lease (newer HLCs, epoch
    /// 0). Whether A pushes its stale renewal blind or pulls first with it
    /// still queued, every device (and one joining later) ends up with B as
    /// the owner, and A stops renewing.
    #[tokio::test]
    async fn network_split_take_over_converges_to_the_taker() {
        use crate::sync::sessions;
        for pull_first in [false, true] {
            let run = format!("split-{}", uuid::Uuid::new_v4().simple());
            let relay = TestRelay::start().await;
            relay.create_group("g1");
            let a = manual_side(&relay, "dev-a").await;
            let b = manual_side(&relay, "dev-b").await;
            let c = manual_side(&relay, "dev-c").await;
            let id_b = crate::sync::ensure_device(&b.0).await.unwrap();
            let project = std::env::temp_dir().join(format!("devdy-split-{}", uuid::Uuid::new_v4().simple()));
            sqlx::query("INSERT INTO projects (id, name, path, created_at) VALUES ('p1', 'p', ?, 'x')")
                .bind(project.to_string_lossy().as_ref())
                .execute(&a.0)
                .await
                .unwrap();
            sqlx::query("INSERT INTO runs (id, project_id, type, status, engine, created_at) VALUES (?, 'p1', 'session', 'done', 'claude', 'x')")
                .bind(&run)
                .execute(&a.0)
                .await
                .unwrap();
            let path = project.to_string_lossy().to_string();
            sessions::claim(&a.0, &run, &path).await.unwrap();
            sessions::release(&a.0, &run).await;
            push_side(&a).await;
            pull_side(&b).await;
            pull_side(&c).await;
            assert_eq!(owner_of(&b.0, &run).await.1, 0);

            // The split: B takes over, then A resumes and renews offline.
            let taken = sessions::take_over(&b.0, &run, false, true, true).await.unwrap();
            assert!(taken.taken);
            assert_eq!(owner_of(&b.0, &run).await, (Some(id_b.clone()), 1, None));
            push_side(&b).await;
            tokio::time::sleep(Duration::from_millis(5)).await;
            sessions::claim(&a.0, &run, &path).await.unwrap();
            tokio::time::sleep(Duration::from_millis(5)).await;
            sessions::renew(&a.0, std::slice::from_ref(&run)).await.unwrap();

            if pull_first {
                // A's renewal is still queued when B's take-over arrives.
                pull_side(&a).await;
                push_side(&a).await;
            } else {
                // A pushes its stale renewal without having pulled.
                push_side(&a).await;
                pull_side(&a).await;
            }
            pull_side(&b).await;
            pull_side(&c).await;
            for (name, side) in [("a", &a), ("b", &b), ("c", &c)] {
                assert_eq!(owner_of(&side.0, &run).await, (Some(id_b.clone()), 1, None), "device {name} (pull_first {pull_first})");
            }
            // A device joining later converges too.
            let d = manual_side(&relay, "dev-d").await;
            pull_side(&d).await;
            assert_eq!(owner_of(&d.0, &run).await, (Some(id_b.clone()), 1, None), "late joiner (pull_first {pull_first})");
            // A no longer renews a run it lost.
            sessions::renew(&a.0, std::slice::from_ref(&run)).await.unwrap();
            assert_eq!(owner_of(&a.0, &run).await.2, None);
        }
    }

    /// Review 3 H1: two upserts of a merged-away id reach A in one pull (B's
    /// and C's concurrent edits of B's copy of a skill A also has under a
    /// lower id). The second one, redirected to A's id, must not drop the
    /// merge's push of the survivor: every device, and one joining later,
    /// ends up with the skill under A's id and the newest edit.
    #[tokio::test]
    async fn two_upserts_of_a_merged_id_in_one_pull_keep_the_survivor() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let a = manual_side(&relay, "dev-a").await;
        let b = manual_side(&relay, "dev-b").await;
        let c = manual_side(&relay, "dev-c").await;
        let exec = |db: &crate::db::Db, sql: &'static str| {
            let db = db.clone();
            async move { sqlx::query(sql).execute(&db).await.unwrap() }
        };
        let skills = |db: &crate::db::Db| {
            let db = db.clone();
            async move {
                sqlx::query_scalar::<_, String>("SELECT id || ':' || description FROM skills").fetch_all(&db).await.unwrap()
            }
        };
        let pause = || tokio::time::sleep(Duration::from_millis(5));
        exec(&a.0, "INSERT INTO skills (id, name, description, source_path, updated_at) VALUES ('sk-1', 'review', 'from a', '/s', 'x')").await;
        pause().await;
        exec(&b.0, "INSERT INTO skills (id, name, description, source_path, updated_at) VALUES ('sk-2', 'review', 'from b', '/s', 'x')").await;
        push_side(&b).await;
        pull_side(&c).await;
        pause().await;
        exec(&b.0, "UPDATE skills SET description = 'b again' WHERE id = 'sk-2'").await;
        push_side(&b).await;
        pause().await;
        // C edits without having pulled B's second edit: the relay keeps both.
        exec(&c.0, "UPDATE skills SET description = 'from c' WHERE id = 'sk-2'").await;
        push_side(&c).await;
        assert_eq!(relay.op_count(), 2);

        pull_side(&a).await;
        assert_eq!(skills(&a.0).await, vec!["sk-1:from c"]);
        push_side(&a).await;
        pull_side(&b).await;
        pull_side(&c).await;
        let d = manual_side(&relay, "dev-d").await;
        pull_side(&d).await;
        for (name, side) in [("a", &a), ("b", &b), ("c", &c), ("d (joined later)", &d)] {
            assert_eq!(skills(&side.0).await, vec!["sk-1:from c"], "device {name}");
        }
    }

    /// Review-3 M6: the relay is restored from a backup and a device that
    /// never saw the lost ops pushes its head past B's cursor before B comes
    /// back. B must still refuse it (needs a re-merge), while a plain relay
    /// restart keeps B syncing.
    #[tokio::test]
    async fn restored_relay_is_refused_after_another_device_pushed_past() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let a = manual_side(&relay, "dev-a").await;
        let b = manual_side(&relay, "dev-b").await;
        let (tx, _rx) = mpsc::unbounded_channel();
        connect_checked(&b.1, tx).await.unwrap();
        let exec = |db: &crate::db::Db, sql: &'static str| {
            let db = db.clone();
            async move { sqlx::query(sql).execute(&db).await.unwrap() }
        };
        exec(&a.0, "INSERT INTO todos (id, text, position, created_at) VALUES ('t1', 'one', 0, 'x')").await;
        push_side(&a).await;
        let backup = relay.snapshot("g1");
        exec(&a.0, "INSERT INTO todos (id, text, position, created_at) VALUES ('t2', 'two', 1, 'x'), ('t3', 'three', 2, 'x')").await;
        push_side(&a).await;
        pull_side(&b).await;
        let cursor = b.1.store.cursor().await.unwrap();
        assert_eq!(b.1.store.relay_mark().await.unwrap().map(|m| m.seen), Some(cursor));

        // A restart is not a restore.
        relay.restart();
        let (tx, _rx) = mpsc::unbounded_channel();
        connect_checked(&b.1, tx).await.expect("a restarted relay keeps its history");

        relay.restore("g1", backup);
        let c = manual_side(&relay, "dev-c").await;
        for sql in [
            "INSERT INTO todos (id, text, position, created_at) VALUES ('c1', 'c', 0, 'x')",
            "INSERT INTO todos (id, text, position, created_at) VALUES ('c2', 'c', 0, 'x')",
            "INSERT INTO todos (id, text, position, created_at) VALUES ('c3', 'c', 0, 'x')",
        ] {
            exec(&c.0, sql).await;
        }
        push_side(&c).await;
        assert!(relay.head() >= cursor, "the head alone no longer shows the loss");
        let (tx, _rx) = mpsc::unbounded_channel();
        let err = connect_checked(&b.1, tx).await.err();
        assert_eq!(err, Some(TransportError::Rejected(crate::sync::transport::RELAY_BEHIND.into())));
    }

    /// Finding #1, the exact scenario: B edits a row while offline (older
    /// HLC), A edits it later and pushes, then B pushes its older edit before
    /// it has pulled A's. Real stores, cycle steps driven by hand so the race
    /// is deterministic. Everyone — including a device joining afterwards —
    /// must end up with A's newer edit.
    #[tokio::test]
    async fn offline_stale_push_converges() {
        use crate::sync::files::tests::mem_db;
        use crate::sync::store::SqliteSyncStore;

        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let mut sides = Vec::new();
        for id in ["dev-a", "dev-b", "dev-c"] {
            let db = mem_db().await;
            let store: Arc<dyn SyncStore> = Arc::new(SqliteSyncStore::new(db.clone(), id.into()));
            let (tx, _rx) = mpsc::unbounded_channel();
            let conn = Connection::connect(&params(&relay, id), false, tx).await.unwrap();
            let ctx = Ctx {
                params: params(&relay, id),
                keys: keys(),
                store,
                shared: Arc::new(Shared::new(Arc::new(|_: &SyncStatus| {}), false, PeerHandlers::default())),
                timing: fast(),
            };
            sides.push((db, ctx, conn));
        }
        let title = |db: &crate::db::Db| {
            let db = db.clone();
            async move {
                sqlx::query_scalar::<_, String>("SELECT text FROM todos WHERE id = 't1'")
                    .fetch_one(&db)
                    .await
                    .unwrap()
            }
        };
        let exec = |db: &crate::db::Db, sql: &'static str| {
            let db = db.clone();
            async move { sqlx::query(sql).execute(&db).await.unwrap() }
        };
        async fn push(side: &(crate::db::Db, Ctx, Connection)) -> Result<(), CycleError> {
            push_all(&side.1, &side.2).await.map(|_| ())
        }
        async fn pull(side: &(crate::db::Db, Ctx, Connection)) -> Result<(), CycleError> {
            let blobs = RelayBlobs::new(&side.2, &side.1.keys);
            pull_all(&side.1, &side.2, &blobs, &|_, _| {}).await.map(|_| ())
        }

        // A creates the row; B has it.
        exec(&sides[0].0, "INSERT INTO todos (id, text, position, created_at) VALUES ('t1', 'v0', 0, 'x')").await;
        push(&sides[0]).await.unwrap();
        pull(&sides[1]).await.unwrap();
        assert_eq!(title(&sides[1].0).await, "v0");

        // B offline edits first (older HLC), A edits later and pushes.
        exec(&sides[1].0, "UPDATE todos SET text = 'from B (t=100)' WHERE id = 't1'").await;
        tokio::time::sleep(Duration::from_millis(5)).await;
        exec(&sides[0].0, "UPDATE todos SET text = 'from A (t=200)' WHERE id = 't1'").await;
        push(&sides[0]).await.unwrap();
        // B comes back and pushes before pulling (the race the relay must survive).
        push(&sides[1]).await.unwrap();
        assert_eq!(relay.op_count(), 2, "A's unseen op is kept next to B's stale one");

        pull(&sides[0]).await.unwrap();
        pull(&sides[1]).await.unwrap();
        pull(&sides[2]).await.unwrap();
        for (i, (db, _, _)) in sides.iter().enumerate() {
            assert_eq!(title(db).await, "from A (t=200)", "device {i}");
        }
    }
}
