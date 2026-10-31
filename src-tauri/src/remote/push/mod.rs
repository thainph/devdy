//! Web Push from the Host to the phone controller PWA
//! (`docs/remote-mobile-push-spec.md`).
//!
//! The Host is its own application server: it signs with a VAPID key kept in
//! the Keychain and POSTs encrypted, count-only messages straight to each
//! browser's push service — the relay is not involved. A dispatcher task, alive
//! while Remote Control runs, listens on the [`RemoteBus`], collapses bursts of
//! prompts / finished runs into one notification per device, and stays quiet
//! while a phone Controller is connected and on screen.

pub mod actions;
pub mod coalesce;
pub mod commands;
pub mod crypto;
pub mod send;
pub mod store;

use crate::db::Db;
use crate::remote::{RemoteBus, RemoteRunEvent};
use coalesce::{compose, kind_of_done, suppressed, Coalescer, PushKind, PushMessage, COALESCE_WINDOW};
use p256::SecretKey;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::broadcast::error::RecvError;

/// Per-request timeout towards a push service.
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// Push runtime state, held in [`crate::remote::RemoteState`].
#[derive(Clone)]
pub struct PushHub {
    /// A phone Controller is connected. A remote desktop (`ipc`) does not
    /// count: it is another computer, not the phone being notified.
    phone_connected: Arc<AtomicBool>,
    /// Whether the connected phone is on screen (`push_visibility`).
    /// Reset to true on every authentication: a freshly opened app is visible.
    visible: Arc<AtomicBool>,
    client: reqwest::Client,
    task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl Default for PushHub {
    fn default() -> Self {
        PushHub {
            phone_connected: Arc::new(AtomicBool::new(false)),
            visible: Arc::new(AtomicBool::new(true)),
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

impl PushHub {
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    pub fn set_visible(&self, visible: bool) {
        self.visible.store(visible, Ordering::SeqCst);
    }

    /// A Controller authenticated: `phone` is false for a remote desktop.
    pub fn controller_attached(&self, phone: bool) {
        self.phone_connected.store(phone, Ordering::SeqCst);
        self.visible.store(true, Ordering::SeqCst);
    }

    /// The Controller's room is gone.
    pub fn controller_detached(&self) {
        self.phone_connected.store(false, Ordering::SeqCst);
    }

    /// Whether pushes are held back right now (a phone is open on screen).
    pub fn is_suppressed(&self) -> bool {
        suppressed(self.phone_connected.load(Ordering::SeqCst), self.visible.load(Ordering::SeqCst))
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
    if let Some(b64) = crate::secrets::get_remote_vapid_key() {
        return crypto::b64url_decode(&b64)
            .and_then(|b| SecretKey::from_slice(&b).ok())
            .ok_or_else(|| "stored VAPID key is unreadable".to_string());
    }
    let key = crypto::generate_secret_key();
    crate::secrets::set_remote_vapid_key(&crypto::b64url(&key.to_bytes())).map_err(|e| e.to_string())?;
    tracing::info!(event = "remote_push_vapid_generated");
    Ok(key)
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

/// Which bus events count, and as what.
fn kind_of(ev: &RemoteRunEvent) -> Option<PushKind> {
    match ev {
        RemoteRunEvent::PermissionRequest { .. } => Some(PushKind::Pending),
        RemoteRunEvent::Done { status, .. } => kind_of_done(status),
        _ => None,
    }
}

async fn dispatch(hub: PushHub, db: Db, bus: RemoteBus) {
    let mut rx = bus.subscribe();
    let mut co = Coalescer::new(COALESCE_WINDOW);
    loop {
        let due = co.deadline();
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(ev) => {
                    if let Some(kind) = kind_of(&ev) {
                        co.note(kind, Instant::now());
                    }
                }
                // Missed events only cost precision in the counts.
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            },
            _ = async {
                match due {
                    Some(d) => tokio::time::sleep_until(tokio::time::Instant::from_std(d)).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                if let Some(counts) = co.take_due(Instant::now()) {
                    if hub.is_suppressed() {
                        tracing::debug!(event = "remote_push_suppressed_foreground");
                        continue;
                    }
                    flush(&hub, &db, counts).await;
                }
            }
        }
    }
}

/// Send one burst to every device that opted into what happened.
async fn flush(hub: &PushHub, db: &Db, counts: coalesce::BurstCounts) {
    let subs = match store::list(db).await {
        Ok(s) if !s.is_empty() => s,
        _ => return,
    };
    let Some(vapid) = existing_vapid_key() else { return };
    let global = store::events(db).await;
    let open_pending = crate::runs::permission::all_pending().len() as u32;
    let subject = subject(db).await;
    for sub in &subs {
        let Some(msg) = compose(counts, open_pending, sub.events.and(global)) else { continue };
        send::deliver(db, hub.client(), &vapid, &subject, sub, &msg).await;
    }
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
        hub.controller_attached(false);
        assert!(!hub.is_suppressed(), "a remote desktop does not count");
        hub.controller_attached(true);
        assert!(hub.is_suppressed(), "a phone just opened the app");
        hub.set_visible(false);
        assert!(!hub.is_suppressed(), "the phone went to the background");
        hub.set_visible(true);
        hub.controller_detached();
        assert!(!hub.is_suppressed(), "the phone disconnected");
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
