//! Relay connection and device-to-device requests (design: two-way-sync.md
//! §3, on-demand-sync.md §2, §P).
//!
//! One task per configured group keeps a relay connection up (reconnect with
//! capped backoff, like `remote::agent`) so this device can serve the other
//! one, but nothing syncs on its own (O1). The relay stores nothing (§P): it
//! forwards sealed [`PeerMsg`]s (`sync_peer_send` / `sync_peer_msg`) between
//! the two devices of a group, and every transfer is pulled by the asker:
//! - "Sync from <device>" ([`EngineHandle::run_from`]) runs [`feed::pull`]:
//!   the source's change feed page by page (`changes`), applied through the
//!   [`SyncStore`], then the content of the files taken (`file_read`).
//! - Requests of the other device are served here: the feed kinds
//!   (`changes`, `file_read`) through the store, [`LEADER_CHANGED`] ("you
//!   lead now — sync from me") by pulling from the sender, and every other
//!   kind by the registered [`PeerHandler`]s. Paused = requests are refused
//!   with [`PEER_PAUSED`]. Long requests send [`STILL_WORKING`] keepalives.
//!
//! The engine implements [`PeerLink`] over the live connection. Status is
//! published through a callback (the app emits it as `sync://status`). Op
//! contents, peer messages and keys are never logged.

use super::crypto::SyncKeys;
use super::feed::{self, PullOptions, CHANGES, FILE_READ};
pub use super::feed::{RunPhase, RunProgress, SyncRunReport, STILL_WORKING};
use super::peer::{PeerHandler, PeerLink, PeerMsg, Replies, Reply, ERROR_KIND};
use super::store::SyncStore;
use super::transport::{ConnectParams, Connection, Event, TransportError};
use async_trait::async_trait;
use base64::Engine as _;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::{mpsc, watch, Notify};

pub const STATUS_EVENT: &str = "sync://status";
/// Progress of a manual sync (`{phase, message?, pages, ops, done_bytes,
/// total_bytes, files_done, files_total}`).
pub const RUN_EVENT: &str = "sync://run";
/// Request served by the engine: "you are the leader now — sync from me". The
/// receiver pulls the sender's changes (rows only), then replies
/// [`LEADER_SYNCED`].
pub const LEADER_CHANGED: &str = "leader_changed";
pub const LEADER_SYNCED: &str = "leader_synced";
/// Too many requests of that device in flight here.
pub const PEER_BUSY: &str = "busy";
/// The requester gave up on its request `reply_to` (dropped the replies or
/// timed out): the source stops serving it.
pub const PEER_CANCEL: &str = "cancel";
/// Requests of one device served at once (a file transfer keeps
/// `peer::CHUNK_WINDOW` in flight); more are answered [`PEER_BUSY`].
const MAX_SERVED_PER_PEER: usize = super::peer::CHUNK_WINDOW + 4;
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
/// Largest sealed peer message (base64 chars) worth sending: the relay's
/// default cap, base64 of `RELAY_SYNC_MAX_BLOB_BYTES` (4 MiB + 64).
pub const MAX_PEER_DATA_CHARS: usize = (4 * 1024 * 1024 + 64usize).div_ceil(3) * 4;

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
    /// Unix millis this device last served the other one's changes (`push`
    /// for the UI) / last pulled from it.
    pub last_push: Option<i64>,
    pub last_pull: Option<i64>,
    /// File / Keychain items pulled but not fetched yet (as of the last sync).
    pub pending: usize,
    /// Rows too large to be served (not synced until they change again).
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

/// Cadences, overridable by tests.
#[derive(Clone)]
pub struct Timing {
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    /// After a fatal rejection (revoked, bad token) retry this rarely.
    pub rejected_retry: Duration,
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
    /// One manual sync (pull) at a time. Never held while serving.
    run: tokio::sync::Mutex<()>,
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
            run: tokio::sync::Mutex::new(()),
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

    /// Parked rows changed outside a sync (retry): show the new count now.
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

    /// Requests and replies to / from the other device.
    pub fn link(&self) -> Arc<dyn PeerLink> {
        Arc::new(EngineLink(self.ctx.clone()))
    }

    /// Devices of the group connected to the relay right now (not this one).
    pub async fn online_peers(&self) -> Result<Vec<String>, String> {
        self.link().online_peers().await
    }

    /// Wait (at most `wait`) until the relay connection is up.
    pub async fn wait_connected(&self, wait: Duration) -> bool {
        let mut rx = self.ctx.shared.conn.subscribe();
        let up = async {
            loop {
                if rx.borrow_and_update().is_some() {
                    return;
                }
                if rx.changed().await.is_err() {
                    std::future::pending::<()>().await;
                }
            }
        };
        tokio::time::timeout(wait, up).await.is_ok()
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

    /// Manual sync from `source`: pull its change feed and the files `opts`
    /// asks for. `progress` sees the non-final phases.
    pub async fn run_from(
        &self,
        source: &str,
        opts: PullOptions,
        progress: feed::ProgressFn<'_>,
    ) -> Result<SyncRunReport, String> {
        run_from(&self.ctx, source, opts, progress).await
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
        let wait = match Connection::connect(&ctx.params, false, ev_tx).await {
            Ok(conn) => {
                backoff = ctx.timing.backoff_min;
                let conn = Arc::new(conn);
                ctx.shared.conn.send_replace(Some(conn.clone()));
                if let Ok(parked) = ctx.store.parked_count().await {
                    ctx.shared.update(|s| s.parked = parked);
                }
                ctx.shared.update(|s| {
                    s.state = SyncState::Online;
                    s.last_error = None;
                });
                tracing::info!(event = "sync_connected");
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

/// Handle the connection's events until it drops or we stop. Never blocks on
/// sync work: requests are served on their own tasks.
async fn serve(ctx: &Arc<Ctx>, mut events: mpsc::UnboundedReceiver<Event>, stop: &mut watch::Receiver<bool>) {
    loop {
        tokio::select! {
            _ = stop.changed() => return,
            ev = events.recv() => match ev {
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
    if data.len() > MAX_PEER_DATA_CHARS {
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
    tracing::debug!(event = "sync_peer_request", kind = %msg.kind);
    let reply: Arc<dyn Reply> = Arc::new(PeerReply {
        ctx: ctx.clone(),
        to: from.clone(),
        reply_to: msg.id.clone(),
    });
    let sent = if ctx.shared.paused() {
        reply.error(PEER_PAUSED).await
    } else if msg.kind == CHANGES || msg.kind == FILE_READ {
        let work = feed::serve(ctx.store.as_ref(), &from, &msg.kind, msg.body);
        match with_keepalive(&ctx, &reply, STILL_WORKING, || json!({}), work).await {
            Ok((kind, body)) => {
                if kind == feed::CHANGES_PAGE {
                    ctx.shared.update(|s| s.last_push = Some(now_ms()));
                }
                reply.send(kind, body, true).await
            }
            Err(e) => reply.error(e).await,
        }
    } else if msg.kind == LEADER_CHANGED {
        // The sender made this device the leader: fetch that now, so its
        // singleton jobs do not wait for the next manual sync. Only the
        // leader setting matters here: rows, no files.
        let synced = run_from(&ctx, &from, PullOptions::rows_only(), &|_, _| {});
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

#[async_trait]
impl PeerLink for EngineLink {
    async fn online_peers(&self) -> Result<Vec<String>, String> {
        let conn = self.0.shared.conn().ok_or_else(|| NOT_CONNECTED.to_string())?;
        conn.peers().await.map_err(code_of)
    }

    async fn request(&self, to: &str, kind: &str, body: Value) -> Result<Replies, String> {
        request(&self.0, to, kind, body).await
    }
}

/// The asking side of a manual sync (one at a time).
async fn run_from(
    ctx: &Arc<Ctx>,
    source: &str,
    opts: PullOptions,
    progress: feed::ProgressFn<'_>,
) -> Result<SyncRunReport, String> {
    if source == ctx.params.device_id {
        return Err("cannot sync from this device itself".into());
    }
    if ctx.shared.conn().is_none() {
        return Err(NOT_CONNECTED.into());
    }
    let _one = ctx.shared.run.lock().await;
    let link: Arc<dyn PeerLink> = Arc::new(EngineLink(ctx.clone()));
    let result = feed::pull(link, ctx.store.as_ref(), source, opts, progress).await;
    match &result {
        Ok(r) => {
            let pending = r.pending_files;
            ctx.shared.update(|s| {
                s.last_pull = Some(now_ms());
                s.pending = pending;
            });
        }
        Err(e) => tracing::info!(event = "sync_run_failed", error = %e),
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::codec::PathCtx;
    use crate::sync::feed::{ChangesReq, FileScope, PeerFiles};
    use crate::sync::files::tests::{mem_db, tempdir, TempDir};
    use crate::sync::store::SqliteSyncStore;
    use crate::sync::test_relay::TestRelay;
    use std::sync::atomic::Ordering;

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

    /// A device on a real SQLite store with its own home folder.
    struct Device {
        id: String,
        db: crate::db::Db,
        home: TempDir,
        paths: PathCtx,
        store: Arc<SqliteSyncStore>,
        engine: SyncEngine,
        handlers: PeerHandlers,
    }

    impl Device {
        async fn sync_from(&self, source: &str) -> Result<SyncRunReport, String> {
            self.engine.handle().run_from(source, PullOptions::all(), &|_, _| {}).await
        }
        fn link(&self) -> Arc<dyn PeerLink> {
            self.engine.handle().link()
        }
        async fn exec(&self, sql: &str) {
            sqlx::query(sql).execute(&self.db).await.unwrap();
        }
        async fn rows(&self, sql: &str) -> Vec<String> {
            sqlx::query_scalar(sql).fetch_all(&self.db).await.unwrap()
        }
        async fn cursor(&self, source: &str) -> u64 {
            self.store.cursor(source).await.unwrap()
        }
        /// A project at `<home>/w/p` with a run `id` whose log is `log`.
        async fn run_with_log(&self, id: &str, log: &[u8]) -> std::path::PathBuf {
            let project = self.home.path().join("w/p");
            std::fs::create_dir_all(project.join(".devdy/runs")).unwrap();
            sqlx::query("INSERT OR IGNORE INTO projects (id, name, path, created_at) VALUES ('p1', 'p', ?, 'x')")
                .bind(project.to_string_lossy().as_ref())
                .execute(&self.db)
                .await
                .unwrap();
            let path = project.join(format!(".devdy/runs/{id}.log"));
            std::fs::write(&path, log).unwrap();
            sqlx::query("INSERT INTO runs (id, project_id, type, status, engine, created_at, output_path) VALUES (?, 'p1', 'session', 'done', 'claude', 'x', ?)")
                .bind(id)
                .bind(path.to_string_lossy().as_ref())
                .execute(&self.db)
                .await
                .unwrap();
            path
        }
        async fn stop(self) {
            self.engine.stop().await;
        }
    }

    async fn device(relay: &TestRelay, id: &str, timing: Timing) -> Device {
        let home = tempdir();
        let db = mem_db().await;
        let paths = PathCtx::with_home(home.path()).with_app_data(home.path().join("data"));
        let store = Arc::new(SqliteSyncStore::new(db.clone(), id.into()).with_paths(paths.clone()));
        let sink: StatusSink = Arc::new(|_: &SyncStatus| {});
        let handlers = PeerHandlers::default();
        let engine = SyncEngine::start_with(params(relay, id), keys(), store.clone(), sink, false, timing, handlers.clone());
        Device { id: id.into(), db, home, paths, store, engine, handlers }
    }

    async fn pair(relay: &TestRelay) -> (Device, Device) {
        relay.create_group("g1");
        let a = device(relay, "dev-a", fast()).await;
        let b = device(relay, "dev-b", fast()).await;
        online(&[&a, &b]).await;
        (a, b)
    }

    async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
        for _ in 0..400 {
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

    async fn gone(d: &Device, peer: &str) {
        for _ in 0..200 {
            if !d.engine.handle().online_peers().await.unwrap().iter().any(|p| p == peer) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("{peer} still online");
    }

    #[tokio::test]
    async fn nothing_syncs_on_its_own() {
        let relay = TestRelay::start().await;
        let (a, b) = pair(&relay).await;
        a.exec("INSERT INTO notes (id, title, position, created_at, updated_at) VALUES ('n1', 'a', 0, 'x', 'x')").await;
        a.engine.kick();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!((relay.forwarded_from("dev-a"), relay.forwarded_from("dev-b")), (0, 0), "no message without a request");
        assert!(b.rows("SELECT id FROM notes").await.is_empty());
        a.stop().await;
        b.stop().await;
    }

    #[tokio::test]
    async fn sync_run_pulls_rows_from_its_cursor_one_direction() {
        let relay = TestRelay::start().await;
        let (a, b) = pair(&relay).await;
        assert_eq!(b.engine.handle().online_peers().await.unwrap(), vec!["dev-a"]);
        a.exec("INSERT INTO notes (id, title, content, position, created_at, updated_at) VALUES ('n1', 'first', 'secret body', 0, 'x', 'x'), ('n2', 'second', '', 1, 'x', 'x')").await;
        b.exec("INSERT INTO notes (id, title, position, created_at, updated_at) VALUES ('n3', 'b own', 2, 'x', 'x')").await;
        let phases = Arc::new(Mutex::new(Vec::new()));
        let seen = phases.clone();
        let report = b
            .engine
            .handle()
            .run_from("dev-a", PullOptions::all(), &move |p, pr| seen.lock().unwrap().push((p, pr.pages)))
            .await
            .unwrap();
        assert_eq!((report.pulled, report.applied, report.pages), (2, 2, 1));
        assert_eq!(b.rows("SELECT id FROM notes ORDER BY id").await, vec!["n1", "n2", "n3"]);
        assert_eq!(b.cursor("dev-a").await, report.last_seq);
        assert!(report.last_seq > 0);
        assert!(a.engine.status().last_push.is_some());
        assert!(b.engine.status().last_pull.is_some());
        // One direction: A has nothing of B's.
        assert_eq!(a.rows("SELECT id FROM notes ORDER BY id").await, vec!["n1", "n2"]);
        let phases = phases.lock().unwrap().clone();
        assert_eq!(phases.first(), Some(&(RunPhase::Requesting, 0)));
        assert!(phases.contains(&(RunPhase::Applying, 1)), "{phases:?}");

        // Nothing new: a second sync is a no-op from the same cursor.
        let again = b.sync_from("dev-a").await.unwrap();
        assert_eq!((again.pulled, again.applied, again.last_seq), (0, 0, report.last_seq));
        // An edit moves the row to the end of A's feed.
        a.exec("UPDATE notes SET title = 'first v2' WHERE id = 'n1'").await;
        let r = b.sync_from("dev-a").await.unwrap();
        assert_eq!((r.pulled, r.applied), (1, 1));
        assert!(r.last_seq > report.last_seq);
        assert_eq!(b.rows("SELECT title FROM notes WHERE id = 'n1'").await, vec!["first v2"]);

        // The other way: A gets B's row, but not its own rows back.
        let page = b.store.changes("dev-a", &ChangesReq::new(0)).await.unwrap();
        assert!(page.ops.iter().all(|o| o.hlc.device != "dev-a"), "the asker's own versions are not served back");
        let r = a.sync_from("dev-b").await.unwrap();
        assert_eq!((r.pulled, r.applied), (1, 1));
        assert_eq!(a.rows("SELECT id FROM notes ORDER BY id").await, vec!["n1", "n2", "n3"]);
        // Each device keeps its own cursor in the other's feed.
        assert_eq!(a.cursor("dev-b").await, r.last_seq);
        assert_eq!(b.cursor("dev-a").await, b.store.cursor("dev-a").await.unwrap());
        a.stop().await;
        b.stop().await;
    }

    #[tokio::test]
    async fn big_feeds_come_page_by_page() {
        let relay = TestRelay::start().await;
        let (a, b) = pair(&relay).await;
        let mut values = Vec::new();
        for i in 0..1200 {
            values.push(format!("('t{i}', 'todo {i}', {i}, 'x')"));
        }
        a.exec(&format!("INSERT INTO todos (id, text, position, created_at) VALUES {}", values.join(", "))).await;
        // Fat rows: the byte budget splits pages too.
        let fat = "x".repeat(300 * 1024);
        for i in 0..6 {
            a.exec(&format!("INSERT INTO notes (id, title, content, position, created_at, updated_at) VALUES ('fat{i}', 'f', '{fat}', {i}, 'x', 'x')")).await;
        }
        let pages = Arc::new(Mutex::new(0u64));
        let seen = pages.clone();
        let r = b
            .engine
            .handle()
            .run_from("dev-a", PullOptions::all(), &move |_, p| *seen.lock().unwrap() = p.pages)
            .await
            .unwrap();
        assert_eq!(r.pulled, 1206);
        assert!(r.pages >= 4, "500 ops and 1 MiB per page: {}", r.pages);
        assert_eq!(*pages.lock().unwrap(), r.pages, "progress counts pages");
        assert_eq!(b.rows("SELECT COUNT(*) || '' FROM todos").await, vec!["1200"]);
        assert_eq!(b.rows("SELECT COUNT(*) || '' FROM notes").await, vec!["6"]);
        a.stop().await;
        b.stop().await;
    }

    /// Rows and file contents over the pipe: a log of several chunks, a
    /// library file; then the log grows (served from the longer live file,
    /// only the growth is read).
    #[tokio::test]
    async fn sync_run_reads_file_contents_chunk_by_chunk() {
        let relay = TestRelay::start().await;
        let (a, b) = pair(&relay).await;
        let mut log: Vec<u8> = (0..(3 * crate::sync::peer::CHUNK_SIZE + 4321)).map(|i| (i % 253) as u8).collect();
        let log_a = a.run_with_log("r1", &log).await;
        let rule = a.paths.app_data.as_ref().unwrap().join("rules/git.md");
        std::fs::create_dir_all(rule.parent().unwrap()).unwrap();
        std::fs::write(&rule, "always rebase").unwrap();
        a.exec(&format!(
            "INSERT INTO rules (id, name, description, source_path, updated_at) VALUES ('ru', 'git', '', '{}', 'x')",
            rule.to_string_lossy()
        ))
        .await;

        let progress = Arc::new(Mutex::new(RunProgress::default()));
        let seen = progress.clone();
        let r = b
            .engine
            .handle()
            .run_from("dev-a", PullOptions::all(), &move |_, p| *seen.lock().unwrap() = p.clone())
            .await
            .unwrap();
        let log_b = b.home.path().join("w/p/.devdy/runs/r1.log");
        assert_eq!(std::fs::read(&log_b).unwrap(), log);
        let rule_b = b.paths.app_data.as_ref().unwrap().join("rules/git.md");
        assert_eq!(std::fs::read_to_string(&rule_b).unwrap(), "always rebase");
        assert_eq!(r.bytes, (log.len() + "always rebase".len()) as u64);
        assert_eq!(r.pending_files, 0);
        let p = progress.lock().unwrap().clone();
        assert_eq!((p.files_done, p.files_total), (2, 2));
        assert_eq!((p.done_bytes, p.total_bytes), (r.bytes, r.bytes));
        assert_eq!(a.store.served_bytes.load(Ordering::Relaxed), r.bytes);
        assert_eq!(b.rows("SELECT id FROM runs").await, vec!["r1"]);

        // A's live log grows: B reads from its last full chunk on.
        log.extend(b"turn 2\n".repeat(1000));
        std::fs::write(&log_a, &log).unwrap();
        let r = b.sync_from("dev-a").await.unwrap();
        assert_eq!(std::fs::read(&log_b).unwrap(), log);
        assert_eq!(r.bytes, (log.len() - 3 * crate::sync::peer::CHUNK_SIZE) as u64);
        a.stop().await;
        b.stop().await;
    }

    /// The source stops serving midway (offline, error): the run ends with
    /// the file still to fetch; the next one resumes after the whole chunks
    /// already written instead of starting over.
    #[tokio::test]
    async fn an_interrupted_file_transfer_resumes() {
        let relay = TestRelay::start().await;
        let (a, b) = pair(&relay).await;
        let chunk = crate::sync::peer::CHUNK_SIZE;
        let log: Vec<u8> = (0..(4 * chunk + 99)).map(|i| (i % 241) as u8).collect();
        a.run_with_log("r1", &log).await;
        *a.store.fail_reads_from.lock().unwrap() = Some(2 * chunk as u64);
        let r = b.sync_from("dev-a").await.unwrap();
        assert_eq!(r.pending_files, 1, "left for the next sync");
        assert_eq!(b.engine.status().pending, 1);
        let log_b = b.home.path().join("w/p/.devdy/runs/r1.log");
        assert!(!log_b.exists());

        *a.store.fail_reads_from.lock().unwrap() = None;
        a.store.served_bytes.store(0, Ordering::Relaxed);
        let r = b.sync_from("dev-a").await.unwrap();
        assert_eq!(r.pending_files, 0);
        assert_eq!(std::fs::read(&log_b).unwrap(), log);
        assert_eq!(a.store.served_bytes.load(Ordering::Relaxed), (log.len() - 2 * chunk) as u64, "resumed");
        a.stop().await;
        b.stop().await;
    }

    /// A rewritten file (library) changed on the source between its listing
    /// and the read: `file_changed`, B keeps what it has and gets the newer
    /// version with the next sync.
    #[tokio::test]
    async fn a_changed_library_file_waits_for_its_newer_op() {
        let relay = TestRelay::start().await;
        let (a, b) = pair(&relay).await;
        let rule = a.paths.app_data.as_ref().unwrap().join("rules/git.md");
        std::fs::create_dir_all(rule.parent().unwrap()).unwrap();
        std::fs::write(&rule, "v1").unwrap();
        a.exec(&format!(
            "INSERT INTO rules (id, name, description, source_path, updated_at) VALUES ('ru', 'git', '', '{}', 'x')",
            rule.to_string_lossy()
        ))
        .await;
        // Pages only: the file op is listed, its content not read yet.
        let opts = PullOptions { files: FileScope::All, fetch: FileScope::Nothing };
        let r = b.engine.handle().run_from("dev-a", opts, &|_, _| {}).await.unwrap();
        assert_eq!(r.pending_files, 1);
        std::fs::write(&rule, "v2, edited since").unwrap();
        let files = PeerFiles::new(b.link(), "dev-a");
        let fetched = b.store.fetch_extra(&files, FileScope::All, &|| {}).await.unwrap();
        assert_eq!((fetched.applied, fetched.skipped_older, fetched.deferred), (0, 1, 0), "dropped, not retried");
        let rule_b = b.paths.app_data.as_ref().unwrap().join("rules/git.md");
        assert!(!rule_b.exists());
        let r = b.sync_from("dev-a").await.unwrap();
        assert_eq!(r.pending_files, 0);
        assert_eq!(std::fs::read_to_string(&rule_b).unwrap(), "v2, edited since");
        a.stop().await;
        b.stop().await;
    }

    /// Selective sync: the source leaves out what the asker does not take;
    /// turning it on resets the cursor, and the next sync brings it.
    #[tokio::test]
    async fn skipped_categories_come_after_a_cursor_reset() {
        let relay = TestRelay::start().await;
        let (a, b) = pair(&relay).await;
        a.exec("INSERT INTO notes (id, title, position, created_at, updated_at) VALUES ('n1', 'note', 0, 'x', 'x')").await;
        a.exec("INSERT INTO todos (id, text, position, created_at) VALUES ('t1', 'todo', 0, 'x')").await;
        a.exec("INSERT INTO mcp_servers (id, name, transport, created_at) VALUES ('m1', 'fs', 'stdio', 'x')").await;
        crate::sync::prefs::set_categories_off(&b.db, &["notes".to_string()]).await.unwrap();
        let r = b.sync_from("dev-a").await.unwrap();
        assert_eq!(r.pulled, 1, "notes and todos left out by the source");
        assert!(b.rows("SELECT id FROM notes UNION ALL SELECT id FROM todos").await.is_empty());
        assert_eq!(b.rows("SELECT id FROM mcp_servers").await, vec!["m1"]);
        assert!(b.cursor("dev-a").await > 0);

        crate::sync::prefs::set_categories_off(&b.db, &[]).await.unwrap();
        assert_eq!(b.cursor("dev-a").await, 0, "turning a category on resets the cursor");
        let r = b.sync_from("dev-a").await.unwrap();
        assert_eq!(b.rows("SELECT id FROM notes UNION ALL SELECT id FROM todos").await, vec!["n1", "t1"]);
        assert_eq!(r.applied, 2, "the rest is already here (LWW)");
        a.stop().await;
        b.stop().await;
    }

    /// Review #5: the old leader tells the new one, which pulls from it at
    /// once (rows only) and confirms; an offline new leader is reported.
    #[tokio::test]
    async fn the_new_leader_pulls_from_the_old_one_when_told() {
        let relay = TestRelay::start().await;
        let (a, b) = pair(&relay).await;
        crate::sync::leader::set_leader(&a.db, Some("dev-b")).await.unwrap();
        a.run_with_log("r1", b"not fetched for a hand-off").await;
        a.engine.handle().notify_leader("dev-b").await.unwrap();
        assert_eq!(crate::sync::leader::leader(&b.db).await.as_deref(), Some("dev-b"));
        assert!(!b.home.path().join("w/p/.devdy/runs/r1.log").exists(), "rows only: no file scan, no transfer");
        assert_eq!(b.rows("SELECT id FROM runs").await, vec!["r1"]);
        assert_eq!(a.engine.handle().notify_leader("dev-z").await, Err(PEER_OFFLINE.to_string()));
        a.stop().await;
        b.stop().await;
    }

    /// Review #4: "Continue here" pulls from the owner first, so a lease the
    /// owner still renews is seen as live (the take-over is refused), and an
    /// unreachable owner is reported instead of read as expired.
    #[tokio::test]
    async fn continue_here_decides_on_the_owners_current_lease() {
        use crate::sync::sessions;
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
        engine_b.handle().run_from(id_a, PullOptions::all(), &|_, _| {}).await.unwrap();
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
        assert_eq!(sessions::refresh_from_owner(Some(engine_b.handle()), id_a).await, Err(PEER_OFFLINE.to_string()));
        assert_eq!(sessions::refresh_from_owner(None, id_a).await, Err(NOT_CONNECTED.to_string()));
        engine_b.stop().await;
        drop((home_a, home_b));
    }

    #[tokio::test]
    async fn sync_from_an_offline_or_paused_device_fails_with_its_code() {
        let relay = TestRelay::start().await;
        let (a, b) = pair(&relay).await;
        assert_eq!(b.sync_from("dev-z").await, Err(PEER_OFFLINE.to_string()));
        assert!(b.sync_from("dev-b").await.is_err(), "not from itself");

        a.exec("INSERT INTO todos (id, text, position, created_at) VALUES ('t1', 'while paused', 0, 'x')").await;
        a.engine.set_paused(true);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(a.engine.status().state, SyncState::Online, "a paused device stays connected");
        assert_eq!(b.sync_from("dev-a").await, Err(PEER_PAUSED.to_string()));
        assert!(b.rows("SELECT id FROM todos").await.is_empty(), "a paused device serves nothing");
        a.engine.set_paused(false);
        b.sync_from("dev-a").await.unwrap();
        assert_eq!(b.rows("SELECT text FROM todos").await, vec!["while paused"]);
        assert!(!a.engine.status().paused);

        let id_b = b.id.clone();
        a.stop().await;
        gone(&b, "dev-a").await;
        assert_eq!(b.sync_from("dev-a").await, Err(PEER_OFFLINE.to_string()));
        assert_eq!(id_b, "dev-b");
        b.stop().await;
    }

    #[tokio::test]
    async fn reconnects_after_the_relay_drops_the_socket() {
        let relay = TestRelay::start().await;
        let (a, b) = pair(&relay).await;
        let hellos = relay.hello_count();
        relay.drop_all();
        eventually("reconnected", || relay.hello_count() >= hellos + 2).await;
        online(&[&a, &b]).await;
        a.exec("INSERT INTO todos (id, text, position, created_at) VALUES ('t1', 'after', 0, 'x')").await;
        for _ in 0..40 {
            if b.sync_from("dev-a").await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(b.rows("SELECT text FROM todos").await, vec!["after"]);
        a.stop().await;
        b.stop().await;
    }

    #[tokio::test]
    async fn revoked_or_third_devices_report_the_error_and_stop_retrying() {
        let relay = TestRelay::start().await;
        let (a, b) = pair(&relay).await;
        // A group takes two devices.
        let c = device(&relay, "dev-c", fast()).await;
        eventually("refused", || c.engine.status().state == SyncState::Error).await;
        assert!(c.engine.status().last_error.unwrap().contains(crate::sync::transport::GROUP_FULL));
        let hellos = relay.hello_count();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(relay.hello_count(), hellos, "no retry storm");
        c.stop().await;

        relay.revoke("g1", "dev-b");
        relay.drop_all();
        eventually("revoked", || b.engine.status().state == SyncState::Error).await;
        assert!(b.engine.status().last_error.unwrap().contains("device_revoked"));
        a.stop().await;
        b.stop().await;
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
        let a = device(&relay, "dev-a", t).await;
        let b = device(&relay, "dev-b", fast()).await;
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
        // The relay only ever forwarded ciphertext.
        a.exec("INSERT INTO notes (id, title, content, position, created_at, updated_at) VALUES ('n1', 't', 'plain secret', 0, 'x', 'x')").await;
        b.sync_from("dev-a").await.unwrap();
        a.stop().await;
        b.stop().await;
    }

    #[tokio::test]
    async fn requests_in_flight_fail_when_the_connection_drops() {
        let relay = TestRelay::start().await;
        let (a, b) = pair(&relay).await;
        b.handlers.write().unwrap().push(Arc::new(Silent(Mutex::new(Vec::new()))));
        let replies = a.link().request("dev-b", "silent", json!({})).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        relay.drop_all();
        let got = collect(replies).await;
        assert_eq!(got.len(), 1);
        assert!(got[0].is_err());
        a.stop().await;
        b.stop().await;
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
        let (a, b) = pair(&relay).await;
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
        a.stop().await;
        b.stop().await;
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
        let a = device(&relay, "dev-a", t).await;
        let b = device(&relay, "dev-b", fast()).await;
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
        a.stop().await;
        b.stop().await;
    }

    /// Review D: a reply that cannot reach the requester (gone) stops the work.
    #[tokio::test]
    async fn work_for_a_requester_that_is_gone_stops() {
        let relay = TestRelay::start().await;
        let (a, b) = pair(&relay).await;
        let dropped = endless(&b, true);
        let replies = a.link().request("dev-b", "endless", json!({})).await.unwrap();
        eventually("served", || !nothing_served(&b)).await;
        // The requester disappears without cancelling (crash / quit).
        std::mem::forget(replies);
        a.stop().await;
        eventually("aborted", || dropped.load(Ordering::SeqCst) && nothing_served(&b)).await;
        b.stop().await;
    }
}
