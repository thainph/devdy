//! Test-only in-process relay implementing the §9.2 sync subset (hello, push
//! with per-key supersede, paginated pull, notify, revoke, blobs, keepalive
//! echo) over a real WebSocket on 127.0.0.1, with the relay's semantics:
//! `id` echoed on replies, `sync_ops.last_seq` = last returned seq while
//! `more` else the head, a failed hello closes the socket, revoke answers
//! `control{code:"ok"}` and kicks the revoked device's connections.

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// Internal outbound marker: close this connection.
const CLOSE: &str = "\u{0}close";

#[derive(Default)]
struct Group {
    head: u64,
    /// (seq, device_id, key, blob)
    ops: Vec<(u64, String, String, String)>,
    devices: HashMap<String, String>,
    revoked: HashSet<String>,
    blobs: HashMap<String, String>,
    released: usize,
}

#[derive(Default)]
struct State {
    groups: HashMap<String, Group>,
    /// conn id → (group, device, sender)
    conns: HashMap<u64, (String, String, mpsc::UnboundedSender<String>)>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    hellos: usize,
    /// (device, limit) of every pull.
    pulls: Vec<(String, u64)>,
    next_conn: u64,
}

#[derive(Clone)]
pub struct TestRelay {
    addr: std::net::SocketAddr,
    state: Arc<Mutex<State>>,
}

impl TestRelay {
    pub const TOKEN: &'static str = "relay-host-token";

    pub async fn start() -> TestRelay {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State::default()));
        let st = state.clone();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let st2 = st.clone();
                let handle = tokio::spawn(async move {
                    if let Ok(ws) = tokio_tungstenite::accept_async(tcp).await {
                        serve_conn(ws, st2).await;
                    }
                });
                st.lock().unwrap().tasks.push(handle);
            }
        });
        TestRelay { addr, state }
    }

    pub fn url(&self) -> String {
        format!("ws://{}/ws", self.addr)
    }

    pub fn create_group(&self, id: &str) {
        self.state.lock().unwrap().groups.entry(id.into()).or_default();
    }

    pub fn revoke(&self, group: &str, device: &str) {
        let mut s = self.state.lock().unwrap();
        s.groups.entry(group.into()).or_default().revoked.insert(device.into());
    }

    /// Store a raw op as if `device` had pushed it.
    pub fn inject(&self, group: &str, device: &str, blob: &str) {
        let mut s = self.state.lock().unwrap();
        let g = s.groups.entry(group.into()).or_default();
        g.head += 1;
        let seq = g.head;
        g.ops.push((seq, device.into(), format!("k{seq}"), blob.into()));
    }

    pub fn op_count(&self) -> usize {
        self.state.lock().unwrap().groups.values().map(|g| g.ops.len()).sum()
    }

    pub fn head(&self) -> u64 {
        self.state.lock().unwrap().groups.values().map(|g| g.head).max().unwrap_or(0)
    }

    pub fn stored_blobs(&self) -> Vec<String> {
        let s = self.state.lock().unwrap();
        s.groups.values().flat_map(|g| g.ops.iter().map(|o| o.3.clone())).collect()
    }

    /// Blobs currently stored / released so far, over all groups.
    pub fn blob_counts(&self) -> (usize, usize) {
        let s = self.state.lock().unwrap();
        (s.groups.values().map(|g| g.blobs.len()).sum(), s.groups.values().map(|g| g.released).sum())
    }

    pub fn hello_count(&self) -> usize {
        self.state.lock().unwrap().hellos
    }

    /// Limits of every pull `device` made.
    pub fn pulls_of(&self, device: &str) -> Vec<u64> {
        let s = self.state.lock().unwrap();
        s.pulls.iter().filter(|p| p.0 == device).map(|p| p.1).collect()
    }

    /// Kill every open socket (relay restart / network drop).
    pub fn drop_all(&self) {
        let mut s = self.state.lock().unwrap();
        for t in s.tasks.drain(..) {
            t.abort();
        }
        s.conns.clear();
    }
}

fn error(code: &str) -> Value {
    json!({ "t": "error", "code": code })
}

async fn serve_conn(
    ws: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    state: Arc<Mutex<State>>,
) {
    // One task owns both halves, so aborting it (drop_all) closes the socket.
    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let conn_id = {
        let mut s = state.lock().unwrap();
        s.next_conn += 1;
        s.next_conn
    };
    let mut me: Option<(String, String)> = None;
    loop {
        tokio::select! {
            out = rx.recv() => {
                let Some(t) = out else { break };
                if t == CLOSE { break; }
                if sink.send(Message::Text(t)).await.is_err() { break; }
            }
            msg = stream.next() => {
                let Some(Ok(msg)) = msg else { break };
                let Message::Text(text) = msg else { continue };
                let Ok(f) = serde_json::from_str::<Value>(&text) else { continue };
                let (reply, close) = handle(&state, conn_id, &tx, &mut me, &f);
                if let Some(mut r) = reply {
                    if !f["id"].is_null() {
                        r["id"] = f["id"].clone();
                    }
                    let _ = tx.send(r.to_string());
                }
                if close {
                    let _ = tx.send(CLOSE.to_string());
                }
            }
        }
    }
    state.lock().unwrap().conns.remove(&conn_id);
}

fn handle(
    state: &Arc<Mutex<State>>,
    conn_id: u64,
    tx: &mpsc::UnboundedSender<String>,
    me: &mut Option<(String, String)>,
    f: &Value,
) -> (Option<Value>, bool) {
    let r = handle_inner(state, conn_id, tx, me, f);
    // Any hello failure closes the connection.
    let close = f["t"] == "sync_hello" && r.as_ref().is_some_and(|v| v["t"] == "error");
    (r, close)
}

fn handle_inner(
    state: &Arc<Mutex<State>>,
    conn_id: u64,
    tx: &mpsc::UnboundedSender<String>,
    me: &mut Option<(String, String)>,
    f: &Value,
) -> Option<Value> {
    let s_ = |k: &str| f[k].as_str().unwrap_or_default().to_string();
    let mut s = state.lock().unwrap();
    let t = f["t"].as_str().unwrap_or_default();
    if t == "keepalive" {
        return Some(json!({ "t": "keepalive", "room_id": "" }));
    }
    if t == "sync_hello" {
        s.hellos += 1;
        if s_("auth_token") != TestRelay::TOKEN {
            return Some(error("auth_failed"));
        }
        let (gid, did, tok) = (s_("group_id"), s_("device_id"), s_("device_token"));
        if f["create"].as_bool() == Some(true) {
            s.groups.entry(gid.clone()).or_default();
        }
        let Some(g) = s.groups.get_mut(&gid) else {
            return Some(error("group_not_found"));
        };
        if g.revoked.contains(&did) {
            return Some(error("device_revoked"));
        }
        match g.devices.get(&did) {
            Some(known) if *known != tok => return Some(error("auth_failed")),
            Some(_) => {}
            None => {
                g.devices.insert(did.clone(), tok);
            }
        }
        let head = g.head;
        s.conns.insert(conn_id, (gid.clone(), did.clone(), tx.clone()));
        *me = Some((gid, did));
        return Some(json!({ "t": "sync_welcome", "last_seq": head }));
    }
    let Some((gid, did)) = me.clone() else {
        return Some(error("forbidden"));
    };
    match t {
        "sync_push" => {
            let g = s.groups.get_mut(&gid)?;
            for item in f["items"].as_array().cloned().unwrap_or_default() {
                let key = item["key"].as_str().unwrap_or_default().to_string();
                let blob = item["blob"].as_str().unwrap_or_default().to_string();
                g.head += 1;
                let seq = g.head;
                g.ops.retain(|o| o.2 != key);
                g.ops.push((seq, did.clone(), key, blob));
            }
            let head = g.head;
            let notify = json!({ "t": "sync_notify", "last_seq": head }).to_string();
            for (id, (cg, _, ctx)) in &s.conns {
                if *id != conn_id && *cg == gid {
                    let _ = ctx.send(notify.clone());
                }
            }
            Some(json!({ "t": "sync_ack", "last_seq": head }))
        }
        "sync_pull" => {
            let after = f["after_seq"].as_u64().unwrap_or(0);
            let limit = f["limit"].as_u64().unwrap_or(500);
            s.pulls.push((did.clone(), limit));
            let g = s.groups.get(&gid)?;
            let newer: Vec<_> = g.ops.iter().filter(|o| o.0 > after).collect();
            let page: Vec<Value> = newer
                .iter()
                .take(limit as usize)
                .map(|o| json!({ "seq": o.0, "device_id": o.1, "blob": o.3 }))
                .collect();
            let more = newer.len() > page.len();
            let last_seq = if more { page.last().map(|p| p["seq"].as_u64().unwrap()).unwrap() } else { g.head };
            Some(json!({ "t": "sync_ops", "items": page, "last_seq": last_seq, "more": more }))
        }
        "sync_revoke" => {
            let target = s_("device_id");
            s.groups.get_mut(&gid)?.revoked.insert(target.clone());
            for (cg, cd, ctx) in s.conns.values() {
                if *cg == gid && *cd == target {
                    let _ = ctx.send(error("device_revoked").to_string());
                    let _ = ctx.send(CLOSE.to_string());
                }
            }
            Some(json!({ "t": "control", "code": "ok" }))
        }
        "blob_put" => {
            let sha = s_("sha");
            s.groups.get_mut(&gid)?.blobs.insert(sha.clone(), s_("data"));
            Some(json!({ "t": "blob_ack", "sha": sha }))
        }
        "blob_release" => {
            // Immediate here (the real relay waits out a grace period).
            let g = s.groups.get_mut(&gid)?;
            let shas: Vec<String> = f["shas"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            let n = shas.iter().filter(|sha| g.blobs.remove(*sha).is_some()).count();
            g.released += n;
            Some(json!({ "t": "blob_released", "released": n }))
        }
        "blob_get" => {
            let sha = s_("sha");
            match s.groups.get(&gid)?.blobs.get(&sha) {
                Some(d) => Some(json!({ "t": "blob_data", "sha": sha, "data": d })),
                None => Some(error("blob_not_found")),
            }
        }
        _ => Some(error("bad_request")),
    }
}
