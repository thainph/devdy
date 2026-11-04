//! Web Push from the Host to the phone controller PWA
//! (`docs/remote-mobile-push-spec.md`).
//!
//! The Host is its own application server: it signs with a VAPID key kept in
//! the Keychain and POSTs encrypted, count-only messages straight to each
//! browser's push service — the relay is not involved. A dispatcher task, alive
//! while Remote Control runs, listens on the [`RemoteBus`], collapses bursts of
//! prompts / finished runs into one notification per device, and stays quiet
//! while a phone Controller is connected and on screen: what happens then is
//! held, dropped if the phone reports leaving (it saw it live), and sent only
//! if it is unclear the phone ever saw it (disconnected, gone quiet).

pub mod actions;
pub mod coalesce;
pub mod commands;
pub mod crypto;
pub mod send;
pub mod store;

use crate::db::Db;
use crate::remote::{RemoteBus, RemoteRunEvent};
use coalesce::{
    compose, kind_of_done, suppressed, BurstCounts, Coalescer, Holder, PushKind, PushMessage, COALESCE_WINDOW,
};
use p256::SecretKey;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{mpsc, Notify};

/// Per-request timeout towards a push service.
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// Push runtime state, held in [`crate::remote::RemoteState`].
#[derive(Clone)]
pub struct PushHub {
    /// A phone Controller is connected.
    phone_connected: Arc<AtomicBool>,
    /// Whether the connected phone is on screen (`push_visibility`).
    /// Reset to true on every authentication: a freshly opened app is visible.
    visible: Arc<AtomicBool>,
    /// When the phone was last heard from (any command, incl. its visibility
    /// heartbeat) — "on screen" goes stale without it.
    last_heard: Arc<Mutex<Instant>>,
    /// Woken when the phone may have stopped looking, to hand out held bursts.
    release: Arc<Notify>,
    /// The phone itself reported leaving the screen: what is held was seen.
    left_screen: Arc<AtomicBool>,
    client: reqwest::Client,
    task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl Default for PushHub {
    fn default() -> Self {
        PushHub {
            phone_connected: Arc::new(AtomicBool::new(false)),
            visible: Arc::new(AtomicBool::new(true)),
            last_heard: Arc::new(Mutex::new(Instant::now())),
            release: Arc::new(Notify::new()),
            left_screen: Arc::new(AtomicBool::new(false)),
            client: reqwest::Client::builder()
                .timeout(SEND_TIMEOUT)
                // A push service never redirects; following one would undo
                // the endpoint vetting.
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_default(),
            task: Arc::new(Mutex::new(None)),
        }
    }
}

/// While bursts are held, how often to re-check whether the phone has gone
/// quiet (no event announces that).
const HOLD_RECHECK: Duration = Duration::from_secs(15);

impl PushHub {
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// The phone said something.
    pub fn touch(&self) {
        *self.last_heard.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }

    pub fn set_visible(&self, visible: bool) {
        self.visible.store(visible, Ordering::SeqCst);
        self.touch();
        if !visible {
            self.left_screen.store(true, Ordering::SeqCst);
            self.release.notify_one();
        }
    }

    /// A Controller authenticated.
    pub fn controller_attached(&self) {
        self.phone_connected.store(true, Ordering::SeqCst);
        self.visible.store(true, Ordering::SeqCst);
        self.touch();
    }

    /// The Controller's room is gone.
    pub fn controller_detached(&self) {
        self.phone_connected.store(false, Ordering::SeqCst);
        self.release.notify_one();
    }

    /// Whether pushes are held back right now (a phone is open on screen).
    pub fn is_suppressed(&self) -> bool {
        let since = self.last_heard.lock().unwrap_or_else(|e| e.into_inner()).elapsed();
        suppressed(self.phone_connected.load(Ordering::SeqCst), self.visible.load(Ordering::SeqCst), since)
    }

    /// Consume a "left the screen" report.
    fn take_left_screen(&self) -> bool {
        self.left_screen.swap(false, Ordering::SeqCst)
    }

    /// Start the dispatcher (no-op if running). Runs alongside the relay agent.
    pub fn start(&self, db: Db, bus: RemoteBus) {
        let mut task = self.task.lock().unwrap_or_else(|e| e.into_inner());
        if task.as_ref().is_some_and(|t| !t.is_finished()) {
            return;
        }
        let hub = self.clone();
        *task = Some(tokio::spawn(dispatch(hub, db, bus)));
    }

    pub fn stop(&self) {
        self.controller_detached();
        if let Some(t) = self.task.lock().unwrap_or_else(|e| e.into_inner()).take() {
            t.abort();
        }
    }
}

/// The VAPID key, generated and stored in the Keychain on first use.
pub fn vapid_key() -> Result<SecretKey, String> {
    let b64 = crate::secrets::get_or_create_remote_vapid_key(|| {
        tracing::info!(event = "remote_push_vapid_generated");
        crypto::b64url(&crypto::generate_secret_key().to_bytes())
    })
    .map_err(|e| e.to_string())?;
    crypto::b64url_decode(&b64)
        .and_then(|b| SecretKey::from_slice(&b).ok())
        .ok_or_else(|| "stored VAPID key is unreadable".to_string())
}

/// The VAPID key only if one exists (the dispatcher never mints one: without
/// it there can be no subscription to send to).
fn existing_vapid_key() -> Option<SecretKey> {
    crate::secrets::get_remote_vapid_key()
        .and_then(|b64| crypto::b64url_decode(&b64))
        .and_then(|b| SecretKey::from_slice(&b).ok())
}

/// The public key a browser subscribes with (`applicationServerKey`).
pub fn vapid_public_key(key: &SecretKey) -> String {
    crypto::b64url(&crypto::public_bytes(&key.public_key()))
}

/// The VAPID `sub` claim for this Host.
pub async fn subject(db: &Db) -> String {
    let contact = crate::remote::get_setting(db, store::KEY_PUSH_CONTACT).await;
    let relay = crate::remote::get_setting(db, crate::remote::KEY_RELAY_URL).await;
    send::vapid_subject(contact.as_deref(), relay.as_deref())
}

/// A remote session's tag on the subscriptions it made: a hash of its
/// session secret (never the secret itself).
pub fn session_tag(session_secret: &str) -> String {
    use sha2::{Digest, Sha256};
    if session_secret.is_empty() {
        return String::new();
    }
    hex::encode(&Sha256::digest(session_secret.as_bytes())[..12])
}

/// Drop the devices a remote session subscribed (it was ended or kicked).
pub async fn forget_session(db: &Db, session_secret: &str) {
    let n = store::remove_session(db, &session_tag(session_secret)).await;
    if n > 0 {
        tracing::info!(event = "remote_push_session_devices_removed", count = n);
    }
}

/// Which bus events count, and as what.
fn kind_of(ev: &RemoteRunEvent) -> Option<PushKind> {
    match ev {
        RemoteRunEvent::PermissionRequest { .. } => Some(PushKind::Pending),
        RemoteRunEvent::Done { status, .. } => kind_of_done(status),
        _ => None,
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(d) => tokio::time::sleep_until(tokio::time::Instant::from_std(d)).await,
        None => std::future::pending::<()>().await,
    }
}

/// Counts bus events into bursts and decides when they go out. Never does I/O
/// itself: sends are handed to [`flush_worker`], so a slow push service can
/// never make this receiver lag (and lose prompts / finished runs).
async fn dispatch(hub: PushHub, db: Db, bus: RemoteBus) {
    let mut rx = bus.subscribe();
    let (tx, work) = mpsc::unbounded_channel::<BurstCounts>();
    // Ends by itself once `tx` is dropped with this task.
    tokio::spawn(flush_worker(hub.clone(), db, work));
    let mut co = Coalescer::new(COALESCE_WINDOW);
    let mut holder = Holder::default();
    // Kept across iterations: the bus is busy (every output line wakes this
    // loop), so a deadline recomputed each pass would never arrive.
    let mut recheck: Option<Instant> = None;
    loop {
        let due = co.deadline();
        if holder.is_holding() {
            recheck.get_or_insert_with(|| Instant::now() + HOLD_RECHECK);
        } else {
            recheck = None;
        }
        // Checked before every use of `holder`, so a held batch can never ride
        // out with a burst after the phone said it saw it.
        let settle = |holder: &mut Holder| {
            if hub.take_left_screen() {
                holder.discard();
            }
        };
        let ready = tokio::select! {
            ev = rx.recv() => {
                match ev {
                    Ok(ev) => {
                        if let Some(kind) = kind_of(&ev) {
                            settle(&mut holder);
                            // On screen, the phone shows it live.
                            if hub.is_suppressed() {
                                holder.hold(kind);
                            } else {
                                co.note(kind, Instant::now());
                            }
                        }
                    }
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!(event = "remote_push_bus_lagged", missed = n);
                    }
                    Err(RecvError::Closed) => break,
                }
                None
            }
            _ = sleep_until(due) => {
                settle(&mut holder);
                co.take_due(Instant::now()).and_then(|counts| holder.on_due(counts, hub.is_suppressed()))
            }
            _ = hub.release.notified() => {
                settle(&mut holder);
                holder.release(hub.is_suppressed())
            }
            _ = sleep_until(recheck) => {
                recheck = None;
                settle(&mut holder);
                holder.release(hub.is_suppressed())
            }
        };
        if let Some(counts) = ready {
            let _ = tx.send(counts);
        }
    }
}

async fn flush_worker(hub: PushHub, db: Db, mut work: mpsc::UnboundedReceiver<BurstCounts>) {
    while let Some(counts) = work.recv().await {
        flush(&hub, &db, counts).await;
    }
}

/// Send one burst to every device that opted into what happened, all at once.
async fn flush(hub: &PushHub, db: &Db, counts: BurstCounts) {
    let Some(vapid) = existing_vapid_key() else { return };
    // A row subscribed with another key can never verify: drop, not retry.
    store::remove_other_keys(db, &vapid_public_key(&vapid)).await;
    let subs = match store::list(db).await {
        Ok(s) if !s.is_empty() => s,
        _ => return,
    };
    let global = store::events(db).await;
    let open_pending = crate::runs::permission::all_pending().len() as u32;
    let subject = subject(db).await;
    let sends = subs.iter().filter_map(|sub| {
        let msg = compose(counts, open_pending, sub.events.and(global))?;
        let (vapid, subject) = (&vapid, &subject);
        Some(async move { send::deliver(db, hub.client(), vapid, subject, sub, &msg).await })
    });
    futures_util::future::join_all(sends).await;
}

/// Send the test notification to one device.
pub async fn send_test(hub: &PushHub, db: &Db, sub: &store::Subscription) -> send::Outcome {
    let vapid = match vapid_key() {
        Ok(k) => k,
        Err(e) => return send::Outcome::Failed(e),
    };
    let subject = subject(db).await;
    send::deliver(db, hub.client(), &vapid, &subject, sub, &PushMessage::test()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn only_a_phone_on_screen_holds_pushes_back() {
        let hub = PushHub::default();
        assert!(!hub.is_suppressed(), "nobody connected");
        hub.controller_attached();
        assert!(hub.is_suppressed(), "a phone just opened the app");
        hub.set_visible(false);
        assert!(!hub.is_suppressed(), "the phone went to the background");
        hub.set_visible(true);
        hub.controller_detached();
        assert!(!hub.is_suppressed(), "the phone disconnected");
    }

    #[tokio::test]
    async fn leaving_the_screen_wakes_the_dispatcher() {
        let hub = PushHub::default();
        hub.controller_attached();
        hub.set_visible(false);
        // A stored permit: the dispatcher sees it even if it was busy.
        tokio::time::timeout(Duration::from_millis(100), hub.release.notified()).await.unwrap();
        assert!(hub.take_left_screen(), "the phone said so: drop what it saw");
        assert!(!hub.take_left_screen(), "consumed once");
        hub.controller_detached();
        tokio::time::timeout(Duration::from_millis(100), hub.release.notified()).await.unwrap();
        assert!(!hub.take_left_screen(), "a lost room is not proof it saw anything");
    }

    #[test]
    fn session_tags_hash_the_secret() {
        let t = session_tag("secret-1");
        assert_eq!(t.len(), 24);
        assert!(!t.contains("secret"));
        assert_eq!(t, session_tag("secret-1"));
        assert_ne!(t, session_tag("secret-2"));
        assert_eq!(session_tag(""), "");
    }

    #[test]
    fn prompts_and_finished_runs_count_but_nothing_else() {
        let req = RemoteRunEvent::PermissionRequest {
            run_id: "r".into(),
            request_id: "q".into(),
            tool: "Bash".into(),
            input: serde_json::Value::Null,
            cwd: None,
            once_only: true,
        };
        assert_eq!(kind_of(&req), Some(PushKind::Pending));
        let done = |status: &str| RemoteRunEvent::Done { run_id: "r".into(), status: status.into() };
        assert_eq!(kind_of(&done("done")), Some(PushKind::Done));
        assert_eq!(kind_of(&done("failed")), Some(PushKind::Failed));
        assert_eq!(kind_of(&done("cancelled")), None);
        let out = RemoteRunEvent::Output { run_id: "r".into(), line: "x".into(), is_stderr: false };
        assert_eq!(kind_of(&out), None);
        let resolved = RemoteRunEvent::PermissionResolved { run_id: "r".into(), request_id: "q".into() };
        assert_eq!(kind_of(&resolved), None);
    }
}
