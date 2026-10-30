//! Background push/pull loop (design §3, §9.3).
//!
//! One task per configured group. It keeps a relay connection up (reconnect
//! with capped backoff, like `remote::agent`) and runs a sync cycle — push
//! every pending outbox op, then pull until `!more`, applying through the
//! [`SyncStore`] — whenever:
//! - it (re)connects,
//! - [`SyncEngine::kick`] is called (debounced), or the 2 s outbox poll sees
//!   pending ops,
//! - the relay sends `sync_notify`,
//! - the 60 s periodic timer fires.
//!
//! Status is published through a callback (the app emits it as
//! `sync://status`). Op contents, blobs and keys are never logged.

use super::crypto::SyncKeys;
use super::op::Op;
use super::store::SyncStore;
use super::transport::{ConnectParams, Connection, Event, TransportError, WireItem};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, watch, Notify};

pub const STATUS_EVENT: &str = "sync://status";
const PUSH_BATCH: usize = 500;
const PUSH_BATCH_BYTES: usize = 4 * 1024 * 1024;

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
        let wait = match Connection::connect(&ctx.params, false, ev_tx).await {
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

    if !cycle_reporting(ctx, conn).await {
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
                Some(Event::Closed) | None => return,
            },
            _ = ctx.shared.kick.notified() => {
                if !ctx.shared.kick_now.swap(false, Ordering::SeqCst) {
                    tokio::time::sleep(ctx.timing.kick_debounce).await;
                }
                true
            }
            _ = poll.tick() => match ctx.store.pending(1).await {
                Ok(p) => !p.is_empty(),
                Err(_) => false,
            },
            _ = periodic.tick() => true,
        };
        if run_cycle && !cycle_reporting(ctx, conn).await {
            return;
        }
    }
}

/// Run a cycle and record the outcome. `false` = the connection is unusable.
async fn cycle_reporting(ctx: &Ctx, conn: &Connection) -> bool {
    match cycle(ctx, conn).await {
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

async fn cycle(ctx: &Ctx, conn: &Connection) -> Result<(), CycleError> {
    push_all(ctx, conn).await?;
    pull_all(ctx, conn).await
}

/// The largest prefix of `ops` that fits one push (≥ 1 op), sealed.
fn build_push(keys: &SyncKeys, ops: &[(i64, Op)]) -> Result<(Vec<WireItem>, i64), CycleError> {
    let mut items = Vec::new();
    let mut bytes = 0usize;
    let mut last_seq = 0i64;
    for (seq, op) in ops {
        let blob = keys
            .seal_op(op)
            .map_err(|e| CycleError::Store(format!("cannot seal op: {e}")))?;
        let key = keys.op_key(&op.tbl, &op.pk);
        let size = blob.len() + key.len() + 32;
        if !items.is_empty() && bytes + size > PUSH_BATCH_BYTES {
            break;
        }
        bytes += size;
        last_seq = *seq;
        items.push(WireItem {
            key: Some(key),
            blob,
            ..Default::default()
        });
    }
    Ok((items, last_seq))
}

async fn push_all(ctx: &Ctx, conn: &Connection) -> Result<(), CycleError> {
    let mut pushed_any = false;
    loop {
        let ops = ctx.store.pending(PUSH_BATCH).await.map_err(CycleError::Store)?;
        ctx.shared.update(|s| s.pending = ops.len());
        if ops.is_empty() {
            break;
        }
        let (items, up_to) = build_push(&ctx.keys, &ops)?;
        let n = items.len();
        conn.push(items).await?;
        ctx.store.mark_pushed(up_to).await.map_err(CycleError::Store)?;
        pushed_any = true;
        tracing::debug!(event = "sync_pushed", ops = n);
    }
    if pushed_any {
        ctx.shared.update(|s| s.last_push = Some(now_ms()));
    }
    Ok(())
}

async fn pull_all(ctx: &Ctx, conn: &Connection) -> Result<(), CycleError> {
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
            match ctx.keys.open_op(&item.blob) {
                Ok(op) => ops.push(op),
                // Tampered or foreign: nothing can ever open it, so skip it
                // rather than wedge the cursor.
                Err(_) => undecryptable += 1,
            }
        }
        if !ops.is_empty() {
            let report = ctx.store.apply_remote(ops).await.map_err(CycleError::Store)?;
            tracing::debug!(
                event = "sync_applied",
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

    #[test]
    fn push_batches_respect_the_byte_cap() {
        let k = keys();
        let big = "x".repeat(PUSH_BATCH_BYTES / 3);
        let ops: Vec<(i64, Op)> = (1..=5).map(|i| (i, op("notes", &format!("n{i}"), &big))).collect();
        let (items, up_to) = build_push(&k, &ops).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(up_to, 2);
        // A single oversized op still goes out alone.
        let huge = vec![(9, op("notes", "n9", &"y".repeat(PUSH_BATCH_BYTES * 2)))];
        let (items, up_to) = build_push(&k, &huge).unwrap();
        assert_eq!((items.len(), up_to), (1, 9));
    }
}
