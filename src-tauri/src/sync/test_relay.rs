//! Test-only in-process relay mirroring the pipe relay of on-demand sync
//! (on-demand-sync.md §P1) over a real WebSocket on 127.0.0.1: it stores no
//! data, only who may connect. Hello with join tickets (a third active device
//! of a group → `group_full`), invite, revoke (only the creator may revoke
//! others; the revoked device's connections are kicked; a creator revoking
//! itself hands the role to its `successor` if active, else the
//! earliest-registered active device, announced with `sync_group_info` and
//! carried by every `sync_welcome` as `creator_device`), `sync_peers`, and
//! `sync_peer_send` (forwarded as `sync_peer_msg` to the target's most recent
//! connection, then `sync_peer_ack`; `peer_offline` / `too_large`), keepalive
//! echo. `id` is echoed on replies, a failed hello closes the socket, the
//! removed store frames answer `unsupported`.
//!
//! Groups made with [`TestRelay::create_group`] are *open* (any device
//! registers on first hello, still at most two) so engine tests need no
//! pairing; groups created by a `create` hello require tickets like the real
//! relay.

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// Internal outbound marker: close this connection.
const CLOSE: &str = "\u{0}close";
/// Active devices a group takes.
const MAX_DEVICES: usize = 2;

#[derive(Default, Clone)]
struct Group {
    devices: HashMap<String, String>,
    /// Device ids in registration order (creator succession).
    joined: Vec<String>,
    revoked: HashSet<String>,
    creator: Option<String>,
    /// Registration without a ticket (test convenience).
    open: bool,
    /// sha256 hex of open join tickets.
    invites: HashSet<String>,
}

impl Group {
    fn active(&self) -> usize {
        self.devices.keys().filter(|d| !self.revoked.contains(*d)).count()
    }
}

#[derive(Default)]
struct State {
    groups: HashMap<String, Group>,
    /// conn id → (group, device, sender)
    conns: HashMap<u64, (String, String, mpsc::UnboundedSender<String>)>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    hellos: usize,
    /// (from, to, data chars) of every forwarded peer message.
    forwarded: Vec<(String, String, usize)>,
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

    pub fn hello_count(&self) -> usize {
        self.state.lock().unwrap().hellos
    }

    /// Peer messages forwarded from `from` (requests and replies alike).
    pub fn forwarded_from(&self, from: &str) -> usize {
        self.state.lock().unwrap().forwarded.iter().filter(|f| f.0 == from).count()
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

/// The relay's default `RELAY_SYNC_MAX_BLOB_BYTES` as base64 chars.
const MAX_PEER_DATA_CHARS: usize = (4 * 1024 * 1024 + 64usize).div_ceil(3) * 4;

fn sha_hex(s: &str) -> String {
    use sha2::Digest as _;
    hex::encode(sha2::Sha256::digest(s.as_bytes()))
}

fn error(code: &str) -> Value {
    json!({ "t": "error", "code": code })
}

async fn serve_conn(ws: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>, state: Arc<Mutex<State>>) {
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
            None if create => return Some(error("group_exists")),
            None if g.open => {
                if g.active() >= MAX_DEVICES {
                    return Some(error("group_full"));
                }
                g.devices.insert(did.clone(), tok);
                g.joined.push(did.clone());
            }
            None => {
                let Some(ticket) = f["ticket"].as_str() else {
                    return Some(error("ticket_required"));
                };
                if !g.invites.contains(&sha_hex(ticket)) {
                    return Some(error("ticket_invalid"));
                }
                // A valid ticket is kept for when there is room again.
                if g.active() >= MAX_DEVICES {
                    return Some(error("group_full"));
                }
                g.invites.remove(&sha_hex(ticket));
                g.devices.insert(did.clone(), tok);
                g.joined.push(did.clone());
            }
        }
        let creator = g.creator.clone();
        s.conns.insert(conn_id, (gid.clone(), did.clone(), tx.clone()));
        *me = Some((gid, did));
        return Some(json!({ "t": "sync_welcome", "creator_device": creator }));
    }
    let Some((gid, did)) = me.clone() else {
        return Some(error("forbidden"));
    };
    match t {
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
        "sync_peers" => {
            let revoked = &s.groups.get(&gid)?.revoked;
            let mut devices: Vec<String> = s
                .conns
                .values()
                .filter(|(cg, cd, _)| *cg == gid && *cd != did && !revoked.contains(cd))
                .map(|(_, cd, _)| cd.clone())
                .collect();
            devices.sort();
            devices.dedup();
            Some(json!({ "t": "sync_peers", "devices": devices }))
        }
        "sync_peer_send" => {
            let (to, data) = (s_("to_device"), s_("data"));
            if to.is_empty() || data.is_empty() {
                return Some(error("bad_request"));
            }
            if data.len() > MAX_PEER_DATA_CHARS {
                return Some(error("too_large"));
            }
            if to == did || s.groups.get(&gid)?.revoked.contains(&to) {
                return Some(error("peer_offline"));
            }
            // The target's most recent live connection.
            let target = s
                .conns
                .iter()
                .filter(|(_, (cg, cd, _))| *cg == gid && *cd == to)
                .max_by_key(|(id, _)| **id)
                .map(|(_, (_, _, ctx))| ctx.clone());
            let Some(target) = target else {
                return Some(error("peer_offline"));
            };
            let len = data.len();
            let msg = json!({ "t": "sync_peer_msg", "from_device": did, "data": data }).to_string();
            if target.send(msg).is_err() {
                return Some(error("peer_offline"));
            }
            s.forwarded.push((did, to, len));
            Some(json!({ "t": "sync_peer_ack" }))
        }
        "sync_push" | "sync_pull" | "blob_put" | "blob_get" | "blob_release" => Some(error("unsupported")),
        _ => Some(error("bad_request")),
    }
}
