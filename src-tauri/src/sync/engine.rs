//! Background push/pull loop (design §3, §9.3).
//!
//! One task per configured group. It keeps a relay connection up (reconnect
//! with capped backoff, like `remote::agent`) and runs a sync cycle — pull
//! until `!more`, applying through the [`SyncStore`], then push every pending
//! outbox op with the pull cursor as `base_seq` — whenever:
//! - it (re)connects,
//! - [`SyncEngine::kick`] is called (debounced), or the 2 s outbox poll sees
//!   pending ops,
//! - the relay sends `sync_notify`,
//! - the 60 s periodic timer fires.
//!
//! Each cycle first lets the store `stage` its non-DB sources (file chunks go
//! up as sealed blobs through [`RelayBlobs`]); pulled `file` / `secret` ops
//! are routed to `apply_remote_extra`, everything else to `apply_remote`.
//!
//! Pulling first means a push is always built on top of everything the relay
//! had, and a push failure never stops pulls. An op the relay can never take
//! (larger than [`MAX_OP_BLOB_CHARS`], or refused as `too_large` /
//! `bad_request`) is parked (`SyncStore::park`) instead of blocking the queue;
//! any other refused push (quota, relay trouble) backs off exponentially up to
//! [`Timing::push_backoff_max`] while pulls go on.
//!
//! Status is published through a callback (the app emits it as
//! `sync://status`). Op contents, blobs and keys are never logged.

use super::crypto::{blob_sha, SyncKeys};
use super::op::Op;
use super::store::{is_extra_tbl, BlobIo, SyncStore};
use async_trait::async_trait;
use base64::Engine as _;
use super::transport::{ConnectParams, Connection, Event, TransportError, WireItem};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch, Notify};

pub const STATUS_EVENT: &str = "sync://status";
const PUSH_BATCH: usize = 500;
const PUSH_BATCH_BYTES: usize = 4 * 1024 * 1024;
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
    /// Unix millis of the last successful push / pull cycle.
    pub last_push: Option<i64>,
    pub last_pull: Option<i64>,
    /// Ops waiting to be pushed (capped at one push batch).
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

/// Cadences, overridable by tests.
#[derive(Clone)]
pub struct Timing {
    pub outbox_poll: Duration,
    pub periodic: Duration,
    pub kick_debounce: Duration,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    /// After a fatal rejection (revoked, bad token) retry this rarely.
    pub rejected_retry: Duration,
    /// Cap of the exponential backoff (from `backoff_min`) between pushes the
    /// relay refused.
    pub push_backoff_max: Duration,
    pub pull_limit: u64,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            outbox_poll: Duration::from_secs(2),
            periodic: Duration::from_secs(60),
            kick_debounce: Duration::from_secs(1),
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(15),
            rejected_retry: Duration::from_secs(300),
            push_backoff_max: Duration::from_secs(600),
            pull_limit: 500,
        }
    }
}

pub type StatusSink = Arc<dyn Fn(&SyncStatus) + Send + Sync>;

struct Shared {
    status: Mutex<SyncStatus>,
    sink: StatusSink,
    kick: Notify,
    /// `sync_now`: skip the debounce.
    kick_now: AtomicBool,
    paused: watch::Sender<bool>,
}

impl Shared {
    fn update(&self, f: impl FnOnce(&mut SyncStatus)) {
        let snapshot = {
            let Ok(mut s) = self.status.lock() else { return };
            f(&mut s);
            s.clone()
        };
        (self.sink)(&snapshot);
    }
}

/// Handle to a running engine. Dropping it does not stop the task; call
/// [`SyncEngine::stop`].
pub struct SyncEngine {
    shared: Arc<Shared>,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl SyncEngine {
    pub fn start(
        params: ConnectParams,
        keys: SyncKeys,
        store: Arc<dyn SyncStore>,
        sink: StatusSink,
        paused: bool,
        timing: Timing,
    ) -> SyncEngine {
        let mut status = SyncStatus::disabled();
        status.paused = paused;
        let (paused_tx, _) = watch::channel(paused);
        let shared = Arc::new(Shared {
            status: Mutex::new(status),
            sink,
            kick: Notify::new(),
            kick_now: AtomicBool::new(false),
            paused: paused_tx,
        });
        let (stop_tx, stop_rx) = watch::channel(false);
        let ctx = Ctx {
            params,
            keys,
            store,
            shared: shared.clone(),
            timing,
        };
        let task = tokio::spawn(run(ctx, stop_rx));
        SyncEngine {
            shared,
            stop: stop_tx,
            task,
        }
    }

    pub fn status(&self) -> SyncStatus {
        self.shared
            .status
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| SyncStatus::disabled())
    }

    /// The outbox changed: sync soon (debounced).
    pub fn kick(&self) {
        self.shared.kick.notify_one();
    }

    /// Sync right away.
    pub fn sync_now(&self) {
        self.shared.kick_now.store(true, Ordering::SeqCst);
        self.shared.kick.notify_one();
    }

    /// Parked rows changed outside a cycle (retry): show the new count now.
    pub fn set_parked(&self, parked: u64) {
        self.shared.update(|s| s.parked = parked);
    }

    pub fn set_paused(&self, paused: bool) {
        self.shared.paused.send_replace(paused);
        self.shared.update(|s| s.paused = paused);
    }

    pub async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(2), self.task).await;
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

async fn run(ctx: Ctx, mut stop: watch::Receiver<bool>) {
    let mut paused_rx = ctx.shared.paused.subscribe();
    let mut backoff = ctx.timing.backoff_min;
    loop {
        if *stop.borrow() {
            break;
        }
        if *paused_rx.borrow() {
            ctx.shared.update(|s| s.state = SyncState::Offline);
            tokio::select! {
                _ = stop.changed() => {}
                _ = paused_rx.changed() => {}
            }
            continue;
        }
        ctx.shared.update(|s| s.state = SyncState::Connecting);
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let connected = match Connection::connect(&ctx.params, false, ev_tx).await {
            // A relay behind our cursor lost ops: stop until re-merged (N15).
            Ok(conn) => match ctx.store.cursor().await {
                Ok(cursor) => conn.check_not_behind(cursor).map(|_| conn),
                Err(e) => Err(TransportError::Network(e)),
            },
            Err(e) => Err(e),
        };
        let wait = match connected {
            Ok(conn) => {
                backoff = ctx.timing.backoff_min;
                ctx.shared.update(|s| s.state = SyncState::Online);
                tracing::info!(event = "sync_connected");
                serve(&ctx, &conn, ev_rx, &mut stop, &mut paused_rx).await;
                drop(conn);
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
            _ = paused_rx.changed() => {}
            // A manual "sync now" also retries immediately.
            _ = ctx.shared.kick.notified() => {}
        }
    }
    ctx.shared.update(|s| s.state = SyncState::Disabled);
}

/// Serve one connection until it drops, we stop, or we pause.
async fn serve(
    ctx: &Ctx,
    conn: &Connection,
    mut events: mpsc::UnboundedReceiver<Event>,
    stop: &mut watch::Receiver<bool>,
    paused_rx: &mut watch::Receiver<bool>,
) {
    let mut poll = tokio::time::interval(ctx.timing.outbox_poll);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut periodic = tokio::time::interval(ctx.timing.periodic);
    periodic.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    periodic.tick().await;

    let mut backoff = PushBackoff::default();
    if !cycle_reporting(ctx, conn, &mut backoff, true).await {
        return;
    }
    loop {
        let run_cycle = tokio::select! {
            _ = stop.changed() => return,
            _ = paused_rx.changed() => {
                if *paused_rx.borrow() { return; }
                true
            }
            ev = events.recv() => match ev {
                Some(Event::Notify(_)) => true,
                Some(Event::Creator(c)) => {
                    if let Err(e) = ctx.store.set_group_creator(&c).await {
                        tracing::warn!(event = "sync_creator_store_failed", error = %e);
                    }
                    false
                }
                Some(Event::Closed) | None => return,
            },
            _ = ctx.shared.kick.notified() => {
                if ctx.shared.kick_now.swap(false, Ordering::SeqCst) {
                    // "Sync now" retries a refused push right away.
                    backoff.reset();
                } else {
                    tokio::time::sleep(ctx.timing.kick_debounce).await;
                }
                true
            }
            _ = poll.tick() => {
                let blobs = RelayBlobs { conn, keys: &ctx.keys };
                if let Err(e) = ctx.store.stage(&blobs).await {
                    tracing::debug!(event = "sync_stage_failed", error = %e);
                }
                // While a refused push backs off, pending ops alone do not
                // trigger a cycle.
                backoff.due() && ctx.store.has_pending().await.unwrap_or(false)
            }
            _ = periodic.tick() => true,
        };
        if run_cycle {
            let push = backoff.due();
            if !cycle_reporting(ctx, conn, &mut backoff, push).await {
                return;
            }
        }
    }
}

/// Delay before pushing again after the relay refused a push.
#[derive(Default)]
struct PushBackoff {
    delay: Option<Duration>,
    until: Option<Instant>,
}

impl PushBackoff {
    fn due(&self) -> bool {
        self.until.is_none_or(|t| Instant::now() >= t)
    }

    fn reset(&mut self) {
        *self = PushBackoff::default();
    }

    /// Back off `backoff_min`, then twice as long each time, up to `max`.
    fn failed(&mut self, min: Duration, max: Duration) -> Duration {
        let d = self.delay.map_or(min, |d| (d * 2).min(max));
        self.delay = Some(d);
        self.until = Some(Instant::now() + d);
        d
    }
}

/// Run a cycle and record the outcome. `false` = the connection is unusable.
async fn cycle_reporting(ctx: &Ctx, conn: &Connection, backoff: &mut PushBackoff, push: bool) -> bool {
    let (pulled, pushed) = cycle(ctx, conn, push).await;
    match &pushed {
        Some(Ok(())) => backoff.reset(),
        Some(Err(e)) if !is_dead(e) => {
            let wait = backoff.failed(ctx.timing.backoff_min, ctx.timing.push_backoff_max);
            tracing::info!(event = "sync_push_backoff", secs = wait.as_secs());
        }
        _ => {}
    }
    if let Ok(parked) = ctx.store.parked_count().await {
        ctx.shared.update(|s| s.parked = parked);
    }
    // A dead socket decides first; then the push error (it is what the user
    // is waiting on), then the pull's.
    let outcome = match (pulled, pushed) {
        (Err(e), _) if is_dead(&e) => Err(e),
        (_, Some(Err(e))) => Err(e),
        (Err(e), _) => Err(e),
        _ => Ok(()),
    };
    match outcome {
        Ok(()) => {
            ctx.shared.update(|s| {
                s.state = SyncState::Online;
                s.last_error = None;
            });
            true
        }
        Err(CycleError::Transport(TransportError::Relay(code))) => {
            // The relay refused one request (quota, oversized op…); the socket is fine.
            let msg = TransportError::Relay(code).to_string();
            ctx.shared.update(|s| {
                s.state = SyncState::Error;
                s.last_error = Some(msg);
            });
            true
        }
        Err(CycleError::Transport(e)) => {
            let msg = e.to_string();
            ctx.shared.update(|s| s.last_error = Some(msg));
            false
        }
        Err(CycleError::Store(e)) => {
            tracing::warn!(event = "sync_store_error", error = %e);
            ctx.shared.update(|s| {
                s.state = SyncState::Error;
                s.last_error = Some(e);
            });
            true
        }
    }
}

#[derive(Debug)]
enum CycleError {
    Transport(TransportError),
    Store(String),
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
}

impl<'a> RelayBlobs<'a> {
    pub fn new(conn: &'a Connection, keys: &'a SyncKeys) -> Self {
        RelayBlobs { conn, keys }
    }
}

#[async_trait]
impl BlobIo for RelayBlobs<'_> {
    async fn put(&self, plaintext: &[u8]) -> Result<String, String> {
        let sealed = self.keys.seal_bytes(plaintext);
        let sha = blob_sha(&sealed);
        let data = base64::engine::general_purpose::STANDARD.encode(&sealed);
        self.conn.blob_put(&sha, data).await.map_err(|e| e.to_string())?;
        Ok(sha)
    }

    async fn get(&self, sha: &str) -> Result<Vec<u8>, String> {
        let data = self.conn.blob_get(sha).await.map_err(|e| e.to_string())?;
        let sealed = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|_| "blob is not valid base64".to_string())?;
        if blob_sha(&sealed) != sha {
            return Err("blob does not match its sha".into());
        }
        self.keys.open_bytes(&sealed).map_err(|e| e.to_string())
    }
}

/// Stage, pull, then (when `push`) push and release blobs. Returns the pull
/// outcome (a staging error counts as one) and the push outcome when it ran.
async fn cycle(
    ctx: &Ctx,
    conn: &Connection,
    push: bool,
) -> (Result<(), CycleError>, Option<Result<(), CycleError>>) {
    let blobs = RelayBlobs { conn, keys: &ctx.keys };
    let staged = ctx.store.stage(&blobs).await.map_err(CycleError::Store);
    let pulled = pull_all(ctx, conn, &blobs).await;
    if pulled.as_ref().is_err_and(is_dead) {
        return (pulled, None);
    }
    let pushed = if push {
        let r = match push_all(ctx, conn).await {
            Ok(()) => release_blobs(ctx, conn).await,
            Err(e) => Err(e),
        };
        Some(r)
    } else {
        None
    };
    (pulled.and(staged), pushed)
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

async fn push_all(ctx: &Ctx, conn: &Connection) -> Result<(), CycleError> {
    let mut pushed_any = false;
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
            Ok(_) => ctx.store.mark_pushed(up_to).await.map_err(CycleError::Store)?,
            // One op spoils the whole (all-or-nothing) batch: send them one by
            // one so only the culprit is parked.
            Err(TransportError::Relay(code)) if is_op_rejection(&code) => {
                for (seq, item) in seqs.into_iter().zip(items) {
                    match conn.push(vec![item], base).await {
                        Ok(_) => ctx.store.mark_pushed(seq).await.map_err(CycleError::Store)?,
                        Err(TransportError::Relay(code)) if is_op_rejection(&code) => park(ctx, seq, &code).await?,
                        Err(e) => return Err(e.into()),
                    }
                }
            }
            Err(e) => return Err(e.into()),
        }
        pushed_any = true;
        tracing::debug!(event = "sync_pushed", ops = n);
    }
    if pushed_any {
        ctx.shared.update(|s| s.last_push = Some(now_ms()));
    }
    Ok(())
}

async fn pull_all(ctx: &Ctx, conn: &Connection, blobs: &RelayBlobs<'_>) -> Result<(), CycleError> {
    let mut cursor = ctx.store.cursor().await.map_err(CycleError::Store)?;
    let mut undecryptable = 0usize;
    loop {
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
        // Rows first: a file op never depends on them, but a run's row
        // landing before its log keeps the UI consistent.
        let (extra, rows): (Vec<Op>, Vec<Op>) = ops.into_iter().partition(|o| is_extra_tbl(&o.tbl));
        if !rows.is_empty() {
            let report = ctx.store.apply_remote(rows).await.map_err(CycleError::Store)?;
            tracing::debug!(
                event = "sync_applied",
                applied = report.applied,
                skipped = report.skipped_older,
                conflicts = report.conflicts,
                deferred = report.deferred,
            );
        }
        if !extra.is_empty() {
            let report = ctx.store.apply_remote_extra(extra, blobs).await.map_err(CycleError::Store)?;
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
    Ok(())
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
            self.inner.lock().unwrap().cursor = seq;
            Ok(())
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
        Timing {
            outbox_poll: Duration::from_millis(50),
            periodic: Duration::from_secs(3600),
            kick_debounce: Duration::from_millis(10),
            backoff_min: Duration::from_millis(50),
            backoff_max: Duration::from_millis(200),
            rejected_retry: Duration::from_secs(3600),
            push_backoff_max: Duration::from_millis(400),
            pull_limit: 500,
        }
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
    }

    fn device(relay: &TestRelay, id: &str, timing: Timing) -> Device {
        let store = Arc::new(FakeStore::default());
        let statuses = Arc::new(Mutex::new(Vec::new()));
        let sink_statuses = statuses.clone();
        let sink: StatusSink = Arc::new(move |s: &SyncStatus| sink_statuses.lock().unwrap().push(s.clone()));
        let engine = SyncEngine::start(params(relay, id), keys(), store.clone(), sink, false, timing);
        Device {
            store,
            engine,
            statuses,
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

    #[tokio::test]
    async fn push_then_notify_pulls_on_the_other_device() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let a = device(&relay, "dev-a", fast());
        let b = device(&relay, "dev-b", fast());
        eventually("both online", || {
            a.engine.status().state == SyncState::Online && b.engine.status().state == SyncState::Online
        })
        .await;

        a.store.add("notes", "n1", "first");
        a.store.add("notes", "n2", "second");
        a.engine.kick();
        eventually("b applied", || b.store.applied_titles().len() == 2).await;
        assert_eq!(b.store.applied_titles(), vec!["first", "second"]);
        assert_eq!(a.store.outbox_len(), 0);
        // A never re-applies its own ops.
        assert!(a.store.applied_titles().is_empty());
        assert!(a.engine.status().last_push.is_some());
        // The relay only ever saw ciphertext.
        assert!(relay.stored_blobs().iter().all(|b| !b.contains("first")));
        a.engine.stop().await;
        b.engine.stop().await;
    }

    #[tokio::test]
    async fn later_op_supersedes_earlier_on_the_relay() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let a = device(&relay, "dev-a", fast());
        a.store.add("notes", "n1", "v1");
        eventually("first push", || relay.op_count() == 1).await;
        a.store.add("notes", "n1", "v2");
        a.engine.kick();
        eventually("second push", || a.store.outbox_len() == 0 && relay.head() == 2).await;
        assert_eq!(relay.op_count(), 1, "same key replaces the older op");

        let b = device(&relay, "dev-b", fast());
        eventually("b applied", || !b.store.applied_titles().is_empty()).await;
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
        for i in 0..10 {
            a.store.add("todos", &format!("t{i}"), &format!("todo {i}"));
        }
        eventually("pushed", || relay.op_count() == 10).await;
        let b = device(&relay, "dev-b", t);
        eventually("b applied all", || b.store.applied_titles().len() == 10).await;
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
        eventually("online", || {
            a.engine.status().state == SyncState::Online && b.engine.status().state == SyncState::Online
        })
        .await;
        let hellos = relay.hello_count();
        relay.drop_all();
        eventually("reconnected", || relay.hello_count() >= hellos + 2).await;
        eventually("online again", || {
            a.engine.status().state == SyncState::Online && b.engine.status().state == SyncState::Online
        })
        .await;
        a.store.add("notes", "n1", "after drop");
        a.engine.kick();
        eventually("b applied", || b.store.applied_titles() == vec!["after drop"]).await;
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
    async fn pause_disconnects_and_resume_catches_up() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let a = device(&relay, "dev-a", fast());
        let b = device(&relay, "dev-b", fast());
        eventually("b online", || b.engine.status().state == SyncState::Online).await;
        b.engine.set_paused(true);
        eventually("b paused", || b.engine.status().state == SyncState::Offline).await;
        a.store.add("notes", "n1", "while paused");
        eventually("pushed", || relay.op_count() == 1).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(b.store.applied_titles().is_empty());
        b.engine.set_paused(false);
        eventually("b caught up", || b.store.applied_titles().len() == 1).await;
        assert!(!b.engine.status().paused);
        a.engine.stop().await;
        b.engine.stop().await;
    }

    #[tokio::test]
    async fn undecryptable_ops_are_skipped_and_reported() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        relay.inject("g1", "dev-x", "garbage-not-a-sealed-op");
        let b = device(&relay, "dev-b", fast());
        eventually("error reported", || {
            b.engine
                .status()
                .last_error
                .is_some_and(|e| e.contains("could not be decrypted"))
        })
        .await;
        assert_eq!(b.store.cursor(), 1, "cursor moves past the bad op");
        b.engine.stop().await;
    }

    /// Real stores on both sides: a run log and a Keychain item created on A
    /// reach B through the relay (blobs sealed, ops routed to files/secrets).
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
        let (hb, db_b, kc_b, _) = &sides[1];

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
        let read_b = || std::fs::read_to_string(&log_b).ok();
        eventually("log on b", || read_b().as_deref() == Some("line 1\n")).await;
        eventually("secret on b", || kc_b.get("github", "acc1").as_deref() == Some("ghp_from_a")).await;
        let out_b: String = sqlx::query_scalar("SELECT output_path FROM runs WHERE id = 'r1'").fetch_one(db_b).await.unwrap();
        assert_eq!(out_b, log_b.to_string_lossy(), "the run row points at the synced file");

        // A live session keeps streaming: appends arrive within the quick cadence.
        std::fs::write(&log, "line 1\nline 2\n").unwrap();
        eventually("append on b", || read_b().as_deref() == Some("line 1\nline 2\n")).await;
        // A's superseded tail chunk is released on the relay once the new
        // manifest is pushed.
        eventually("old tail released", || relay.blob_counts().1 >= 1).await;
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_blob_release").fetch_one(db_a).await.unwrap();
        assert_eq!(left, 0);
        // Nothing but sealed bytes on the relay.
        assert!(relay.stored_blobs().iter().all(|b| !b.contains("ghp_from_a")));
        // B does not echo what it applied.
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
        b.reset();
        assert!(b.due());
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
        b.store.add("notes", "n1", "small 1");
        b.store.add("notes", "big", &"x".repeat(4000));
        b.store.add("notes", "n2", "small 2");
        b.engine.kick();
        eventually("b's outbox drained", || b.store.outbox_len() == 0).await;
        assert_eq!(b.store.parked(), vec![("big".to_string(), "too_large".to_string())]);
        eventually("a got the rest", || a.store.applied_titles() == vec!["small 1", "small 2"]).await;
        eventually("parked in status", || b.engine.status().parked == 1).await;
        // B still pulls.
        a.store.add("notes", "n3", "from a");
        a.engine.kick();
        eventually("b pulled", || b.store.applied_titles() == vec!["from a"]).await;
        a.engine.stop().await;
        b.engine.stop().await;
    }

    /// Finding #4: a push refused for a non-op reason (quota) backs off
    /// instead of re-sending every poll, while pulls go on.
    #[tokio::test]
    async fn refused_push_backs_off_and_pulls_continue() {
        let relay = TestRelay::start().await;
        relay.create_group("g1");
        let a = device(&relay, "dev-a", fast());
        let b = device(&relay, "dev-b", fast());
        eventually("online", || b.engine.status().state == SyncState::Online).await;
        relay.fail_pushes_of("dev-b", Some("quota_exceeded"));
        b.store.add("notes", "n1", "stuck");
        b.engine.kick();
        eventually("error", || b.engine.status().last_error.is_some_and(|e| e.contains("quota_exceeded"))).await;
        let start = relay.push_attempts("dev-b");
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let tries = relay.push_attempts("dev-b") - start;
        // Poll every 50 ms would be ~24; backoff 50→100→200→400→400… is ≤ 6.
        assert!(tries <= 6, "{tries} pushes in 1.2 s");
        a.store.add("notes", "n2", "from a");
        a.engine.kick();
        eventually("b pulled anyway", || b.store.applied_titles() == vec!["from a"]).await;
        relay.fail_pushes_of("dev-b", None);
        eventually("pushed after recovery", || b.store.outbox_len() == 0).await;
        eventually("a got it", || a.store.applied_titles() == vec!["stuck"]).await;
        a.engine.stop().await;
        b.engine.stop().await;
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
                shared: Arc::new(Shared {
                    status: Mutex::new(SyncStatus::disabled()),
                    sink: Arc::new(|_: &SyncStatus| {}),
                    kick: Notify::new(),
                    kick_now: AtomicBool::new(false),
                    paused: watch::channel(false).0,
                }),
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
            push_all(&side.1, &side.2).await
        }
        async fn pull(side: &(crate::db::Db, Ctx, Connection)) -> Result<(), CycleError> {
            let blobs = RelayBlobs { conn: &side.2, keys: &side.1.keys };
            pull_all(&side.1, &side.2, &blobs).await
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
