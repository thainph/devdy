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

use std::collections::HashSet;
use std::sync::Arc;

use remote_e2e::Session;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::Mutex;

use crate::remote::forwarder::{seal_stream, SeqCounter};
use crate::remote::outbound::OutboundTx;
use crate::remote::protocol::StreamPayload;

/// Feature flag of a remote-desktop Controller (see module docs).
pub const FEATURE_IPC: &str = "ipc";
/// Label of the hidden bridge webview.
pub const BRIDGE_LABEL: &str = "remote-bridge";
/// Event the bridge receives requests on.
const BRIDGE_REQUEST_EVENT: &str = "remote-bridge://request";

/// Where replies go: the authenticated IPC Controller's room.
struct Route {
    session: Arc<Session>,
    room_id: String,
    out: OutboundTx,
    seq: Arc<SeqCounter>,
}

#[derive(Default)]
struct Inner {
    route: Option<Route>,
    bridge_ready: bool,
    queue: Vec<serde_json::Value>,
    events: HashSet<String>,
}

/// Shared IPC state (cheap to clone).
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

impl IpcHub {
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
        {
            let mut inner = self.0.lock().await;
            inner.route = Some(Route { session, room_id, out, seq });
            inner.events.clear();
            inner.queue.clear();
        }
        // Start from a clean bridge: drop any subscription a previous
        // Controller left behind.
        let _ = app.emit_to(BRIDGE_LABEL, BRIDGE_REQUEST_EVENT, serde_json::json!({ "type": "reset" }));
        ensure_bridge(app);
    }

    /// The IPC Controller left: stop routing and unsubscribe everything.
    pub async fn detach(&self, app: &AppHandle) {
        let had = {
            let mut inner = self.0.lock().await;
            inner.events.clear();
            inner.queue.clear();
            inner.route.take().is_some()
        };
        if had {
            let _ = app.emit_to(BRIDGE_LABEL, BRIDGE_REQUEST_EVENT, serde_json::json!({ "type": "reset" }));
        }
    }

    /// Hand one request to the bridge (queued until it is ready).
    pub async fn forward(&self, app: &AppHandle, request: serde_json::Value) {
        let mut inner = self.0.lock().await;
        match request.get("type").and_then(|t| t.as_str()) {
            Some("listen") => {
                if let Some(e) = request.get("event").and_then(|e| e.as_str()) {
                    inner.events.insert(e.to_string());
                }
            }
            Some("unlisten") => {
                if let Some(e) = request.get("event").and_then(|e| e.as_str()) {
                    inner.events.remove(e);
                }
            }
            _ => {}
        }
        if inner.bridge_ready && app.get_webview_window(BRIDGE_LABEL).is_some() {
            drop(inner);
            let _ = app.emit_to(BRIDGE_LABEL, BRIDGE_REQUEST_EVENT, request);
        } else {
            // listen/unlisten are replayed from `events`; only queue invokes.
            if request.get("type").and_then(|t| t.as_str()) == Some("invoke") {
                inner.queue.push(request);
            }
            inner.bridge_ready = false;
            drop(inner);
            ensure_bridge(app);
        }
    }

    /// The bridge page (re)loaded.
    pub async fn bridge_ready(&self) -> BridgeHello {
        let mut inner = self.0.lock().await;
        inner.bridge_ready = true;
        BridgeHello {
            requests: std::mem::take(&mut inner.queue),
            events: inner.events.iter().cloned().collect(),
        }
    }

    /// Seal + send a payload from the bridge to the Controller. Dropped when
    /// no IPC Controller is attached.
    pub async fn reply(&self, payload: StreamPayload) {
        let inner = self.0.lock().await;
        let Some(r) = inner.route.as_ref() else { return };
        if let StreamPayload::IpcEvent { event, .. } = &payload {
            // A late event for something the Controller already dropped.
            if !inner.events.contains(event) {
                return;
            }
        }
        if let Some(env) = seal_stream(&r.session, &r.room_id, &payload, r.seq.next()) {
            // Results and events must arrive in order and complete: never shed.
            let _ = r.out.send(env);
        }
    }
}

/// Create the hidden bridge webview if it is not running.
fn ensure_bridge(app: &AppHandle) {
    if app.get_webview_window(BRIDGE_LABEL).is_some() {
        return;
    }
    let built = WebviewWindowBuilder::new(app, BRIDGE_LABEL, WebviewUrl::App("bridge.html".into()))
        .title("Devdy remote bridge")
        .visible(false)
        .build();
    if let Err(e) = built {
        tracing::warn!(event = "remote_bridge_create_failed", error = %e);
    }
}

// ---- Tauri commands (called by the bridge page only) ----

#[tauri::command]
pub async fn remote_bridge_ready(state: tauri::State<'_, crate::remote::RemoteState>) -> Result<BridgeHello, String> {
    Ok(state.ipc.bridge_ready().await)
}

#[tauri::command]
pub async fn remote_bridge_reply(
    state: tauri::State<'_, crate::remote::RemoteState>,
    id: String,
    ok: bool,
    value: Option<serde_json::Value>,
    error: Option<serde_json::Value>,
) -> Result<(), String> {
    state.ipc.reply(StreamPayload::IpcResult { id, ok, value, error }).await;
    Ok(())
}

#[tauri::command]
pub async fn remote_bridge_event(
    state: tauri::State<'_, crate::remote::RemoteState>,
    event: String,
    payload: serde_json::Value,
) -> Result<(), String> {
    state.ipc.reply(StreamPayload::IpcEvent { event, payload }).await;
    Ok(())
}
