//! Test-only in-process relay implementing the §9.2 sync subset (hello with
//! join tickets, invite, push with cursor-aware per-key supersede, paginated
//! pull, notify, revoke, blobs, keepalive echo) over a real WebSocket on
//! 127.0.0.1, with the relay's semantics: `id` echoed on replies,
//! `sync_ops.last_seq` = last returned seq while `more` else the head, a
//! failed hello closes the socket, revoke answers `control{code:"ok"}` (only
//! the creator may revoke others) and kicks the revoked device's connections,
//! and a creator revoking itself hands the role to its `successor` (if active)
//! or the earliest-registered active device, announced with `sync_group_info`
//! and carried by every `sync_welcome` as `creator_device`.
//!
//! Groups made with [`TestRelay::create_group`] are *open* (any device
//! registers on first hello) so engine tests need no pairing; groups created
//! by a `create` hello require tickets like the real relay.

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
    /// Device ids in registration order (creator succession).
    joined: Vec<String>,
    revoked: HashSet<String>,
    creator: Option<String>,
    /// Registration without a ticket (test convenience).
    open: bool,
    /// sha256 hex of open join tickets.
    invites: HashSet<String>,
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
    /// Every push attempt, by device.
    pushes: Vec<String>,
    /// `too_large` for any pushed blob longer than this.
    max_blob: Option<usize>,
    /// device → error code its pushes get.
    fail_pushes: HashMap<String, String>,
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

    /// An open group: devices register on first hello, no ticket.
    pub fn create_group(&self, id: &str) {
        self.state.lock().unwrap().groups.entry(id.into()).or_default().open = true;
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

    /// Refuse pushes carrying a blob longer than `n` chars with `too_large`.
    pub fn reject_blobs_over(&self, n: usize) {
        self.state.lock().unwrap().max_blob = Some(n);
    }

    /// Answer every push of `device` with `error{code}` (`None` = stop).
    pub fn fail_pushes_of(&self, device: &str, code: Option<&str>) {
        let mut s = self.state.lock().unwrap();
        match code {
            Some(c) => s.fail_pushes.insert(device.into(), c.into()),
            None => s.fail_pushes.remove(device),
        };
    }

    /// Push attempts (accepted or not) `device` made.
    pub fn push_attempts(&self, device: &str) -> usize {
        self.state.lock().unwrap().pushes.iter().filter(|d| *d == device).count()
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

fn sha_hex(s: &str) -> String {
    use sha2::Digest as _;
    hex::encode(sha2::Sha256::digest(s.as_bytes()))
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
        let create = f["create"].as_bool() == Some(true);
        if create && !s.groups.contains_key(&gid) {
            let g = s.groups.entry(gid.clone()).or_default();
            g.creator = Some(did.clone());
            g.devices.insert(did.clone(), tok.clone());
            g.joined.push(did.clone());
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
            None if g.open => {
                g.devices.insert(did.clone(), tok);
                g.joined.push(did.clone());
            }
            None if create => return Some(error("group_exists")),
            None => {
                let Some(ticket) = f["ticket"].as_str() else {
                    return Some(error("ticket_required"));
                };
                if !g.invites.remove(&sha_hex(ticket)) {
                    return Some(error("ticket_invalid"));
                }
                g.devices.insert(did.clone(), tok);
                g.joined.push(did.clone());
            }
        }
        let (head, creator) = (g.head, g.creator.clone());
        s.conns.insert(conn_id, (gid.clone(), did.clone(), tx.clone()));
        *me = Some((gid, did));
        return Some(json!({ "t": "sync_welcome", "last_seq": head, "creator_device": creator }));
    }
    let Some((gid, did)) = me.clone() else {
        return Some(error("forbidden"));
    };
    match t {
        "sync_push" => {
            s.pushes.push(did.clone());
            if let Some(code) = s.fail_pushes.get(&did) {
                return Some(error(code));
            }
            let items = f["items"].as_array().cloned().unwrap_or_default();
            if let Some(max) = s.max_blob {
                if items.iter().any(|i| i["blob"].as_str().unwrap_or_default().len() > max) {
                    return Some(error("too_large"));
                }
            }
            let g = s.groups.get_mut(&gid)?;
            let base = f["base_seq"].as_u64().unwrap_or(0);
            for item in items {
                let key = item["key"].as_str().unwrap_or_default().to_string();
                let blob = item["blob"].as_str().unwrap_or_default().to_string();
                g.head += 1;
                let seq = g.head;
                // Only ops the pusher has seen (or pushed itself) are superseded.
                g.ops.retain(|o| o.2 != key || (o.0 > base && o.1 != did));
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
                .map(|o| json!({ "seq": o.0, "device_id": o.1, "key": o.2, "blob": o.3 }))
                .collect();
            let more = newer.len() > page.len();
            let last_seq = if more { page.last().map(|p| p["seq"].as_u64().unwrap()).unwrap() } else { g.head };
            Some(json!({ "t": "sync_ops", "items": page, "last_seq": last_seq, "more": more }))
        }
        "sync_invite" => {
            let ttl = f["ttl_secs"].as_u64().unwrap_or(600);
            if ttl == 0 || ttl > 600 || s_("ticket_hash").len() != 64 {
                return Some(error("bad_request"));
            }
            s.groups.get_mut(&gid)?.invites.insert(s_("ticket_hash"));
            Some(json!({ "t": "control", "code": "ok" }))
        }
        "sync_revoke" => {
            let target = s_("device_id");
            let g = s.groups.get_mut(&gid)?;
            if target != did && g.creator.as_deref() != Some(did.as_str()) && !g.open {
                return Some(error("forbidden"));
            }
            g.revoked.insert(target.clone());
            let moved = if g.creator.as_deref() == Some(target.as_str()) {
                let active = |d: &String| g.devices.contains_key(d) && !g.revoked.contains(d);
                let wanted = f["successor"].as_str().map(str::to_string).filter(|w| active(w));
                g.creator = wanted.or_else(|| g.joined.iter().find(|d| active(d)).cloned());
                g.creator.clone()
            } else {
                None
            };
            let info = moved.map(|c| json!({ "t": "sync_group_info", "creator_device": c }).to_string());
            for (cg, cd, ctx) in s.conns.values() {
                if *cg != gid {
                    continue;
                }
                if *cd == target {
                    let _ = ctx.send(error("device_revoked").to_string());
                    let _ = ctx.send(CLOSE.to_string());
                } else if let Some(info) = &info {
                    let _ = ctx.send(info.clone());
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
