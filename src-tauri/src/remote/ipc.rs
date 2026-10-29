//! Remote desktop IPC: drive this app's backend from another Devdy over the
//! relay, exactly as the local UI does.
//!
//! A Controller that advertises [`FEATURE_IPC`] is another Devdy window running
//! the normal UI with its `invoke` / `listen` routed here. The agent does not
//! interpret those calls: it hands them to a hidden `remote-bridge` webview,
//! which runs the real `invoke` / `listen` — so every command (and every plugin
//! command) behaves byte-for-byte as if the local user had triggered it — and
//! sends results and events back through [`IpcHub::reply`].
//!
//! Requests that arrive before the bridge page has loaded are queued and handed
//! over by `remote_bridge_ready`; the set of subscribed events is kept here too,
//! so a reloaded bridge re-subscribes and a dropped Controller unsubscribes all.
//! The bridge is kept alive: closing it only hides it, a destroyed one is
//! recreated, and a (re)loading page marks it not-ready so requests queue. An
//! invoke handed to a page that then goes away fails with
//! [`ERR_BRIDGE_RELOADED`] instead of hanging on the Controller.
//!
//! Order: the bridge sends results and events through ONE serialized queue,
//! one `remote_bridge_batch` at a time, and each batch is sealed + queued here
//! under a lock, so the Controller sees them in the order the bridge did.
//!
//! Backpressure: results are never dropped; events are sheddable
//! ([`OutboundTx::offer`]). When one was shed the Controller is told, once the
//! link has drained, to refetch its state ([`StreamPayload::IpcResync`]).
//!
//! Being driven is never silent: attach/detach emit [`ATTACHED_EVENT`] /
//! [`DETACHED_EVENT`] for the local "being controlled" indicator and are
//! audited (individual invokes are not — too chatty).

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use remote_e2e::Session;
use serde::{Deserialize, Serialize};
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use crate::remote::forwarder::{seal_stream, SeqCounter};
use crate::remote::outbound::{OutboundTx, OUTBOUND_HIGH_WATER};
use crate::remote::protocol::StreamPayload;

/// Feature flag of a remote-desktop Controller (see module docs).
pub const FEATURE_IPC: &str = "ipc";
/// Label of the hidden bridge webview.
pub const BRIDGE_LABEL: &str = "remote-bridge";
/// Event the bridge receives requests on.
const BRIDGE_REQUEST_EVENT: &str = "remote-bridge://request";
/// App event: an IPC Controller started driving this Devdy (`{ since_unix }`).
pub const ATTACHED_EVENT: &str = "remote://desktop-attached";
/// App event: the IPC Controller left or was cut off (`{}`).
pub const DETACHED_EVENT: &str = "remote://desktop-detached";
/// Rejection of an invoke whose bridge page went away before answering.
pub const ERR_BRIDGE_RELOADED: &str = "remote_bridge_reloaded";
/// How often a pending resync checks whether the link has drained.
const RESYNC_POLL: Duration = Duration::from_millis(250);
/// The resync goes out once the outbound queue is back below this depth.
const RESYNC_BELOW: usize = OUTBOUND_HIGH_WATER / 2;
/// Delay before recreating a destroyed bridge (lets an app exit finish first).
const RECREATE_DELAY: Duration = Duration::from_millis(500);

/// Where replies go: the authenticated IPC Controller's room.
struct Route {
    session: Arc<Session>,
    room_id: String,
    out: OutboundTx,
    seq: Arc<SeqCounter>,
}

impl Route {
    fn seal(&self, payload: &StreamPayload) -> Option<crate::remote::protocol::Envelope> {
        seal_stream(&self.session, &self.room_id, payload, self.seq.next())
    }
}

#[derive(Default)]
struct Inner {
    route: Option<Route>,
    /// When the current IPC Controller attached (Unix secs).
    since: Option<u64>,
    bridge_ready: bool,
    queue: Vec<serde_json::Value>,
    events: HashSet<String>,
    /// Invokes handed to the current bridge page, awaiting their result.
    inflight: HashSet<String>,
    /// An event was shed and the Controller has not been told to resync yet.
    resync_pending: bool,
}

impl Inner {
    fn reset(&mut self) {
        self.events.clear();
        self.queue.clear();
        self.inflight.clear();
        self.resync_pending = false;
    }

    /// Fail every invoke the bridge page took and will never answer.
    fn fail_inflight(&mut self) {
        let ids: Vec<String> = self.inflight.drain().collect();
        let Some(r) = self.route.as_ref() else { return };
        for id in ids {
            let payload = StreamPayload::IpcResult {
                id,
                ok: false,
                value: serde_json::Value::Null,
                error: Some(serde_json::Value::String(ERR_BRIDGE_RELOADED.to_string())),
            };
            if let Some(env) = r.seal(&payload) {
                let _ = r.out.send(env);
            }
        }
    }
}

/// Shared IPC state (cheap to clone). A plain mutex: nothing awaits while
/// holding it, and the bridge's window callbacks (sync) need it too.
#[derive(Clone, Default)]
pub struct IpcHub(Arc<Mutex<Inner>>);

/// What a (re)loaded bridge needs to resume.
#[derive(Serialize)]
pub struct BridgeHello {
    /// Requests that arrived before the bridge was ready, in order.
    pub requests: Vec<serde_json::Value>,
    /// Events the Controller is subscribed to.
    pub events: Vec<String>,
}

/// One item of a `remote_bridge_batch` (see `src/lib/ipc/bridgeOutbox.ts`).
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BridgeItem {
    Result {
        id: String,
        ok: bool,
        #[serde(default)]
        value: serde_json::Value,
        #[serde(default)]
        error: Option<serde_json::Value>,
    },
    Event {
        event: String,
        #[serde(default)]
        payload: serde_json::Value,
    },
}

impl IpcHub {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// An IPC Controller authenticated on `room_id`: route replies there and
    /// make sure the bridge exists.
    pub async fn attach(
        &self,
        app: &AppHandle,
        session: Arc<Session>,
        room_id: String,
        out: OutboundTx,
        seq: Arc<SeqCounter>,
    ) {
        let since = crate::remote::agent::now_unix();
        self.set_route(Route { session, room_id: room_id.clone(), out, seq }, since);
        // Start from a clean bridge: drop any subscription a previous
        // Controller left behind.
        let _ = app.emit_to(BRIDGE_LABEL, BRIDGE_REQUEST_EVENT, serde_json::json!({ "type": "reset" }));
        self.ensure_bridge(app);
        let _ = app.emit(ATTACHED_EVENT, serde_json::json!({ "since_unix": since }));
        tracing::info!(event = "remote_desktop_attached", room_id = %room_id);
        audit_lifecycle(app, &room_id, "desktop_attach").await;
    }

    fn set_route(&self, route: Route, since: u64) {
        let mut inner = self.lock();
        inner.route = Some(route);
        inner.since = Some(since);
        inner.reset();
    }

    /// The IPC Controller left: stop routing and unsubscribe everything.
    pub async fn detach(&self, app: &AppHandle) {
        let had = {
            let mut inner = self.lock();
            inner.reset();
            inner.since = None;
            inner.route.take().map(|r| r.room_id)
        };
        if let Some(room_id) = had {
            let _ = app.emit_to(BRIDGE_LABEL, BRIDGE_REQUEST_EVENT, serde_json::json!({ "type": "reset" }));
            let _ = app.emit(DETACHED_EVENT, serde_json::json!({}));
            tracing::info!(event = "remote_desktop_detached", room_id = %room_id);
            audit_lifecycle(app, &room_id, "desktop_detach").await;
        }
    }

    /// When the attached IPC Controller started driving, or `None` when no
    /// one is.
    pub async fn controlled_since(&self) -> Option<u64> {
        self.lock().since
    }

    /// Hand one request to the bridge (queued until it is ready).
    pub async fn forward(&self, app: &AppHandle, request: serde_json::Value) {
        let deliver = {
            let ready = app.get_webview_window(BRIDGE_LABEL).is_some();
            self.accept_request(&request, ready)
        };
        if deliver {
            let _ = app.emit_to(BRIDGE_LABEL, BRIDGE_REQUEST_EVENT, request);
        } else {
            self.ensure_bridge(app);
        }
    }

    /// Bookkeeping of one request; `true` when it should be emitted to the
    /// bridge now (else it is queued / replayed once the bridge is ready).
    fn accept_request(&self, request: &serde_json::Value, window_exists: bool) -> bool {
        let mut inner = self.lock();
        let kind = request.get("type").and_then(|t| t.as_str());
        let event = request.get("event").and_then(|e| e.as_str());
        match (kind, event) {
            (Some("listen"), Some(e)) => {
                inner.events.insert(e.to_string());
            }
            (Some("unlisten"), Some(e)) => {
                inner.events.remove(e);
            }
            _ => {}
        }
        let is_invoke = kind == Some("invoke");
        if inner.bridge_ready && window_exists {
            if is_invoke {
                if let Some(id) = request.get("id").and_then(|i| i.as_str()) {
                    inner.inflight.insert(id.to_string());
                }
            }
            true
        } else {
            // listen/unlisten are replayed from `events`; only queue invokes.
            if is_invoke {
                inner.queue.push(request.clone());
            }
            inner.bridge_ready = false;
            false
        }
    }

    /// The bridge page (re)loaded.
    pub async fn bridge_ready(&self) -> BridgeHello {
        self.take_hello()
    }

    fn take_hello(&self) -> BridgeHello {
        let mut inner = self.lock();
        inner.bridge_ready = true;
        let requests = std::mem::take(&mut inner.queue);
        for req in &requests {
            if let Some(id) = req.get("id").and_then(|i| i.as_str()) {
                inner.inflight.insert(id.to_string());
            }
        }
        BridgeHello { requests, events: inner.events.iter().cloned().collect() }
    }

    /// The bridge page started (re)loading or its window is gone: queue new
    /// requests, and fail the invokes the old page will never answer.
    fn bridge_lost(&self) {
        let mut inner = self.lock();
        inner.bridge_ready = false;
        inner.fail_inflight();
    }

    /// Seal + queue one batch from the bridge, in order. Dropped when no IPC
    /// Controller is attached.
    pub fn reply_batch(&self, items: Vec<BridgeItem>) {
        if self.deliver(items) {
            self.watch_resync();
        }
    }

    /// [`Self::reply_batch`] minus the spawn; `true` when a resync watcher
    /// should start (the first shed event since the last resync).
    fn deliver(&self, items: Vec<BridgeItem>) -> bool {
        let mut inner = self.lock();
        let inner = &mut *inner;
        let Some(r) = inner.route.as_ref() else { return false };
        let mut start_watch = false;
        for item in items {
            match item {
                BridgeItem::Result { id, ok, value, error } => {
                    // Unknown: already failed (bridge reloaded) or a stale page.
                    if !inner.inflight.remove(&id) {
                        continue;
                    }
                    let payload = StreamPayload::IpcResult { id, ok, value, error };
                    if let Some(env) = r.seal(&payload) {
                        // A result is the only answer to a call: never shed.
                        let _ = r.out.send(env);
                    }
                }
                BridgeItem::Event { event, payload } => {
                    // A late event for something the Controller already dropped.
                    if !inner.events.contains(&event) {
                        continue;
                    }
                    let Some(env) = r.seal(&StreamPayload::IpcEvent { event, payload }) else { continue };
                    if r.out.offer(env) == Ok(false) && !inner.resync_pending {
                        inner.resync_pending = true;
                        start_watch = true;
                    }
                }
            }
        }
        start_watch
    }

    /// Once the link has drained, tell the Controller to refetch (one
    /// `ipc_resync` per congestion episode).
    fn watch_resync(&self) {
        let hub = self.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(RESYNC_POLL).await;
                if hub.try_send_resync() {
                    return;
                }
            }
        });
    }

    /// `true` when done (sent, or nothing to do any more).
    fn try_send_resync(&self) -> bool {
        let mut inner = self.lock();
        if !inner.resync_pending {
            return true;
        }
        let Some(r) = inner.route.as_ref() else {
            inner.resync_pending = false;
            return true;
        };
        if r.out.depth() >= RESYNC_BELOW {
            return false;
        }
        if let Some(env) = r.seal(&StreamPayload::IpcResync) {
            let _ = r.out.send(env);
        }
        tracing::info!(event = "remote_desktop_resync");
        inner.resync_pending = false;
        true
    }

    /// Create the hidden bridge webview if it is not running.
    fn ensure_bridge(&self, app: &AppHandle) {
        if app.get_webview_window(BRIDGE_LABEL).is_some() {
            return;
        }
        let on_load = self.clone();
        let built = WebviewWindowBuilder::new(app, BRIDGE_LABEL, WebviewUrl::App("bridge.html".into()))
            .title("Devdy remote bridge")
            .visible(false)
            .on_page_load(move |_, payload| {
                // Requests sent now would hit a page that is going away.
                if payload.event() == PageLoadEvent::Started {
                    on_load.bridge_lost();
                }
            })
            .build();
        let win = match built {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!(event = "remote_bridge_create_failed", error = %e);
                return;
            }
        };
        let hub = self.clone();
        let app = app.clone();
        let handle = win.clone();
        win.on_window_event(move |event| match event {
            // Nothing local should end the bridge while it may be serving
            // (an app exit destroys it regardless).
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = handle.hide();
            }
            WindowEvent::Destroyed => {
                hub.bridge_lost();
                let hub = hub.clone();
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(RECREATE_DELAY).await;
                    // Still driven, and the app is not quitting (closing `main` quits).
                    let attached = hub.lock().route.is_some();
                    if attached && app.get_webview_window("main").is_some() {
                        hub.ensure_bridge(&app);
                    }
                });
            }
            _ => {}
        });
    }
}

/// Audit an attach/detach (the DB is app-managed; skipped if not yet set up).
async fn audit_lifecycle(app: &AppHandle, room_id: &str, action: &str) {
    let Some(db) = app.try_state::<crate::db::Db>().map(|d| d.inner().clone()) else { return };
    crate::remote::audit::audit(&db, None, Some(room_id), None, action, crate::remote::audit::RESULT_ACCEPTED, None).await;
}

// ---- Tauri commands (called by the bridge page only) ----

#[tauri::command]
pub async fn remote_bridge_ready(state: tauri::State<'_, crate::remote::RemoteState>) -> Result<BridgeHello, String> {
    Ok(state.ipc.bridge_ready().await)
}

/// Results and events, in the order the bridge produced them. The bridge sends
/// one batch at a time, so batches are handled in order too.
#[tauri::command]
pub async fn remote_bridge_batch(
    state: tauri::State<'_, crate::remote::RemoteState>,
    items: Vec<BridgeItem>,
) -> Result<(), String> {
    state.ipc.reply_batch(items);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::outbound::{channel, OutboundRx};
    use remote_e2e::Keypair;

    /// A hub routed to a fake room; returns the Controller's end to open frames.
    fn routed() -> (IpcHub, OutboundRx, Session) {
        let a = Keypair::generate();
        let b = Keypair::generate();
        let psk = b"ipc-test-psk-0123456789";
        let host = Session::new(psk, &a, &b.public).unwrap();
        let ctrl = Session::new(psk, &b, &a.public).unwrap();
        let (out, rx) = channel();
        let hub = IpcHub::default();
        let route = Route { session: Arc::new(host), room_id: "rm_1".into(), out, seq: Arc::new(SeqCounter::default()) };
        hub.set_route(route, 1);
        (hub, rx, ctrl)
    }

    fn drain(rx: &mut OutboundRx, ctrl: &Session) -> Vec<(u64, serde_json::Value)> {
        let mut got = Vec::new();
        while let Ok(env) = rx.try_recv() {
            let plain = ctrl.open(env.cipher.as_deref().unwrap()).unwrap();
            got.push((env.seq.unwrap(), serde_json::from_slice(&plain).unwrap()));
        }
        got
    }

    fn invoke(hub: &IpcHub, id: &str) {
        assert!(hub.accept_request(&serde_json::json!({ "type": "invoke", "id": id, "cmd": "x", "args": {} }), true));
    }

    fn listen(hub: &IpcHub, event: &str) {
        hub.accept_request(&serde_json::json!({ "type": "listen", "event": event }), true);
    }

    fn result(id: &str, value: serde_json::Value) -> BridgeItem {
        BridgeItem::Result { id: id.into(), ok: true, value, error: None }
    }

    fn event(name: &str, n: u64) -> BridgeItem {
        BridgeItem::Event { event: name.into(), payload: serde_json::json!(n) }
    }

    #[test]
    fn batch_keeps_results_and_events_in_order_with_rising_seq() {
        let (hub, mut rx, ctrl) = routed();
        hub.take_hello();
        listen(&hub, "run://status");
        invoke(&hub, "a");
        invoke(&hub, "b");
        assert!(!hub.deliver(vec![result("a", serde_json::json!(1)), event("run://status", 7), result("b", serde_json::json!(2))]));
        let got = drain(&mut rx, &ctrl);
        let kinds: Vec<String> = got
            .iter()
            .map(|(_, v)| format!("{}:{}", v["kind"].as_str().unwrap(), v.get("id").or(v.get("event")).unwrap()))
            .collect();
        assert_eq!(kinds, ["ipc_result:\"a\"", "ipc_event:\"run://status\"", "ipc_result:\"b\""]);
        assert!(got.windows(2).all(|w| w[0].0 < w[1].0), "seq must rise in send order");
    }

    #[test]
    fn a_null_result_value_is_serialized() {
        let (hub, mut rx, ctrl) = routed();
        hub.take_hello();
        invoke(&hub, "a");
        hub.deliver(vec![result("a", serde_json::Value::Null)]);
        let got = drain(&mut rx, &ctrl);
        let obj = got[0].1.as_object().unwrap();
        assert_eq!(obj.get("value"), Some(&serde_json::Value::Null));
    }

    #[test]
    fn unknown_results_and_unsubscribed_events_are_dropped() {
        let (hub, mut rx, ctrl) = routed();
        hub.take_hello();
        hub.deliver(vec![result("nope", serde_json::json!(1)), event("not-listened", 1)]);
        assert!(drain(&mut rx, &ctrl).is_empty());
    }

    #[test]
    fn shed_events_trigger_one_resync_after_the_link_drains() {
        let (hub, mut rx, ctrl) = routed();
        hub.take_hello();
        listen(&hub, "e");
        invoke(&hub, "a");
        let flood: Vec<BridgeItem> = (0..(OUTBOUND_HIGH_WATER as u64 + 10)).map(|n| event("e", n)).collect();
        assert!(hub.deliver(flood), "first shed starts the watcher");
        // Results are never shed, even above the mark; further sheds don't
        // start another watcher.
        assert!(!hub.deliver(vec![event("e", 0), result("a", serde_json::json!("ok"))]));
        assert!(!hub.try_send_resync(), "still congested");
        let got = drain(&mut rx, &ctrl);
        assert_eq!(got.len(), OUTBOUND_HIGH_WATER + 1);
        assert_eq!(got.last().unwrap().1["kind"], "ipc_result");
        assert!(hub.try_send_resync());
        let got = drain(&mut rx, &ctrl);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1["kind"], "ipc_resync");
        assert!(hub.try_send_resync(), "nothing pending any more");
        assert!(drain(&mut rx, &ctrl).is_empty());
    }

    #[test]
    fn a_reloading_bridge_fails_its_inflight_invokes_but_keeps_the_queue() {
        let (hub, mut rx, ctrl) = routed();
        hub.take_hello();
        listen(&hub, "e");
        invoke(&hub, "a");
        hub.bridge_lost();
        // Not ready any more: queued, not delivered.
        let queued = serde_json::json!({ "type": "invoke", "id": "b", "cmd": "x", "args": {} });
        assert!(!hub.accept_request(&queued, true));
        let got = drain(&mut rx, &ctrl);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1["id"], "a");
        assert_eq!(got[0].1["ok"], false);
        assert_eq!(got[0].1["error"], ERR_BRIDGE_RELOADED);
        // The old page's late answer is ignored.
        hub.deliver(vec![result("a", serde_json::json!(1))]);
        assert!(drain(&mut rx, &ctrl).is_empty());
        // The new page gets the queue and the subscriptions; "b" is now in flight.
        let hello = hub.take_hello();
        assert_eq!(hello.requests, vec![queued]);
        assert_eq!(hello.events, vec!["e".to_string()]);
        hub.deliver(vec![result("b", serde_json::json!(2))]);
        assert_eq!(drain(&mut rx, &ctrl)[0].1["value"], 2);
    }

    #[test]
    fn batch_items_deserialize_from_the_bridge_shape() {
        let items: Vec<BridgeItem> = serde_json::from_value(serde_json::json!([
            { "kind": "result", "id": "a", "ok": true, "value": null, "error": null },
            { "kind": "event", "event": "e", "payload": { "x": 1 } },
        ]))
        .unwrap();
        assert!(matches!(&items[0], BridgeItem::Result { id, value, .. } if id == "a" && value.is_null()));
        assert!(matches!(&items[1], BridgeItem::Event { event, .. } if event == "e"));
    }
}
