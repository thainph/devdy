//! Integration tests for sync groups (design `on-demand-sync` §P1: the relay
//! is a pipe) over a real in-process WebSocket, each relay with its own temp
//! SQLite file.

use std::path::PathBuf;
use std::time::Duration;

use devdy_relay::config::Config;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

const TEST_TOKEN: &str = "test-secret-token-1234567890";
const GROUP: &str = "grp-1";

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

struct Opts {
    sync: bool,
    rate_limit_max: u32,
    /// Store file (default: a fresh temp file).
    db: Option<PathBuf>,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            sync: true,
            rate_limit_max: 1000,
            db: None,
        }
    }
}

fn temp_db() -> PathBuf {
    std::env::temp_dir().join(format!("devdy-relay-sync-{}.db", uuid::Uuid::new_v4().simple()))
}

async fn start(opts: Opts) -> String {
    let config = Config {
        bind: "127.0.0.1:0".to_string(),
        host_token: TEST_TOKEN.to_string(),
        rate_limit_max: opts.rate_limit_max,
        rate_limit_window: Duration::from_secs(10),
        ping_interval: Duration::from_secs(15),
        peer_liveness_timeout: Duration::from_secs(45),
        max_rooms: 256,
        max_message_bytes: 32 * 1024 * 1024,
        sync_db: opts.sync.then(|| opts.db.unwrap_or_else(temp_db)),
        sync_max_peer_bytes: 4 * 1024 * 1024 + 64,
    };
    let (handle, accept_loop) = devdy_relay::server::serve(config).await.expect("relay must bind");
    tokio::spawn(accept_loop);
    format!("ws://{}", handle.local_addr)
}

async fn send(ws: &mut Ws, v: Value) {
    ws.send(Message::Text(v.to_string())).await.expect("send");
}

async fn recv_within(ws: &mut Ws, wait: Duration) -> Option<Value> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        match tokio::time::timeout_at(deadline, ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => return serde_json::from_str(&t).ok(),
            Ok(Some(Ok(Message::Close(_)))) => return None,
            Ok(Some(Ok(_))) => continue,
            _ => return None,
        }
    }
}

async fn recv(ws: &mut Ws) -> Value {
    recv_within(ws, Duration::from_secs(5)).await.expect("expected a frame")
}

/// The socket was closed by the relay (no further frame).
async fn assert_closed(ws: &mut Ws) {
    assert!(recv_within(ws, Duration::from_secs(3)).await.is_none(), "connection should be closed");
}

async fn hello_raw(url: &str, device: &str, token: &str, create: bool) -> (Ws, Value) {
    let (mut ws, _) = connect_async(url).await.expect("connect");
    send(
        &mut ws,
        json!({ "t": "sync_hello", "auth_token": TEST_TOKEN, "group_id": GROUP,
                "device_id": device, "device_token": token, "create": create }),
    )
    .await;
    let reply = recv(&mut ws).await;
    (ws, reply)
}

async fn hello(url: &str, device: &str, create: bool) -> Ws {
    let (ws, reply) = hello_raw(url, device, &format!("tok-{device}"), create).await;
    assert_eq!(reply["t"], "sync_welcome", "got {reply}");
    ws
}

/// Register a join ticket for `device` through `member`; returns the ticket.
async fn invite(member: &mut Ws, device: &str) -> String {
    let ticket = format!("ticket-for-{device}");
    send(member, json!({ "t": "sync_invite", "ticket_hash": sha_hex(ticket.as_bytes()), "ttl_secs": 60 })).await;
    let ack = recv(member).await;
    assert_eq!((ack["t"].as_str(), ack["code"].as_str()), (Some("control"), Some("ok")), "got {ack}");
    ticket
}

/// Join `device` to the group with a ticket that `member` registers.
async fn join(url: &str, member: &mut Ws, device: &str) -> Ws {
    let ticket = invite(member, device).await;
    let (ws, reply) = hello_ticket(url, device, &ticket).await;
    assert_eq!(reply["t"], "sync_welcome", "got {reply}");
    ws
}

async fn hello_ticket(url: &str, device: &str, ticket: &str) -> (Ws, Value) {
    let (mut ws, _) = connect_async(url).await.expect("connect");
    send(
        &mut ws,
        json!({ "t": "sync_hello", "auth_token": TEST_TOKEN, "group_id": GROUP,
                "device_id": device, "device_token": format!("tok-{device}"), "ticket": ticket }),
    )
    .await;
    let reply = recv(&mut ws).await;
    (ws, reply)
}

fn sha_hex(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

#[tokio::test]
async fn sync_disabled_without_db() {
    let url = start(Opts { sync: false, ..Default::default() }).await;
    let (mut ws, reply) = hello_raw(&url, "a", "t", true).await;
    assert_eq!(reply["t"], "error");
    assert_eq!(reply["code"], "sync_disabled");
    assert_closed(&mut ws).await;

    let (mut ws, _) = connect_async(&url).await.unwrap();
    send(&mut ws, json!({ "t": "sync_peers" })).await;
    assert_eq!(recv(&mut ws).await["code"], "sync_disabled");
}

#[tokio::test]
async fn hello_create_ticket_and_auth() {
    let url = start(Opts::default()).await;

    // Unknown group without create.
    let (mut ws, r) = hello_raw(&url, "a", "tok-a", false).await;
    assert_eq!(r["code"], "group_not_found");
    assert_closed(&mut ws).await;

    // Bad shared token.
    let (mut ws, _) = connect_async(&url).await.unwrap();
    send(
        &mut ws,
        json!({ "t": "sync_hello", "auth_token": "nope", "group_id": GROUP, "device_id": "a",
                "device_token": "tok-a", "create": true }),
    )
    .await;
    assert_eq!(recv(&mut ws).await["code"], "auth_failed");
    assert_closed(&mut ws).await;

    let (mut a, w) = hello_raw(&url, "a", "tok-a", true).await;
    assert_eq!(w["t"], "sync_welcome");
    assert_eq!(w["creator_device"], "a");
    for gone in ["last_seq", "epoch", "known_head"] {
        assert!(w.get(gone).is_none(), "welcome has no {gone}: {w}");
    }
    // An unknown device needs a ticket; create=true does not bypass it.
    let (mut x, r) = hello_raw(&url, "x", "tok-x", false).await;
    assert_eq!(r["code"], "ticket_required");
    assert_closed(&mut x).await;
    let (mut x, r) = hello_raw(&url, "x", "tok-x", true).await;
    assert_eq!(r["code"], "group_exists");
    assert_closed(&mut x).await;
    let (mut x, r) = hello_ticket(&url, "x", "made-up").await;
    assert_eq!(r["code"], "ticket_invalid");
    assert_closed(&mut x).await;
    // Second device joins with a ticket, then its token alone works again.
    let b = join(&url, &mut a, "b").await;
    drop(b);
    let _b = hello(&url, "b", false).await;
    // The ticket was one-time.
    let (mut x, r) = hello_ticket(&url, "x", "ticket-for-b").await;
    assert_eq!(r["code"], "ticket_invalid");
    assert_closed(&mut x).await;
    // Known device, wrong token.
    let (mut bad, r) = hello_raw(&url, "b", "stolen", false).await;
    assert_eq!(r["code"], "auth_failed");
    assert_closed(&mut bad).await;
    // create=true on an existing group is an idempotent hello.
    let _a2 = hello(&url, "a", true).await;

    // A welcomed connection cannot hello again.
    send(&mut a, json!({ "t": "sync_hello", "auth_token": TEST_TOKEN, "group_id": GROUP,
                         "device_id": "a", "device_token": "tok-a" }))
    .await;
    assert_eq!(recv(&mut a).await["code"], "forbidden");
}

#[tokio::test]
async fn a_third_device_gets_group_full() {
    let url = start(Opts::default()).await;
    let mut a = hello(&url, "a", true).await;
    let mut b = join(&url, &mut a, "b").await;

    let ticket = invite(&mut a, "c").await;
    let (mut c, r) = hello_ticket(&url, "c", &ticket).await;
    assert_eq!((r["t"].as_str(), r["code"].as_str()), (Some("error"), Some("group_full")), "got {r}");
    assert_closed(&mut c).await;
    // The two members are unaffected.
    let _a2 = hello(&url, "a", false).await;
    let _b2 = hello(&url, "b", false).await;

    // Once b leaves, c takes the free slot with the same (unused) ticket.
    send(&mut b, json!({ "t": "sync_revoke", "device_id": "b" })).await;
    assert_eq!(recv(&mut b).await["code"], "ok");
    let (_c, r) = hello_ticket(&url, "c", &ticket).await;
    assert_eq!(r["t"], "sync_welcome", "got {r}");
}

#[tokio::test]
async fn removed_frames_are_unsupported() {
    let url = start(Opts::default()).await;
    let removed = [
        json!({ "t": "sync_push", "id": 1, "items": [{ "key": "k", "blob": "x" }] }),
        json!({ "t": "sync_pull", "after_seq": 0 }),
        json!({ "t": "blob_put", "sha": sha_hex(b"x"), "data": "eA==" }),
        json!({ "t": "blob_get", "sha": sha_hex(b"x") }),
        json!({ "t": "blob_release", "shas": [] }),
        json!({ "t": "sync_notify", "last_seq": 1 }),
        json!({ "t": "sync_ops" }),
    ];
    // Before and after sync_hello; the connection stays usable.
    let (mut fresh, _) = connect_async(&url).await.unwrap();
    let mut a = hello(&url, "a", true).await;
    for ws in [&mut fresh, &mut a] {
        for f in &removed {
            send(ws, f.clone()).await;
            let e = recv(ws).await;
            assert_eq!((e["t"].as_str(), e["code"].as_str()), (Some("error"), Some("unsupported")), "{f} -> {e}");
        }
    }
    send(&mut a, json!({ "t": "sync_push", "id": 7 })).await;
    assert_eq!(recv(&mut a).await["id"], 7, "request id is echoed");
    assert!(peers(&mut a).await.is_empty());
}

#[tokio::test]
async fn peer_send_is_not_rate_limited() {
    let url = start(Opts { rate_limit_max: 3, ..Default::default() }).await;
    let mut a = hello(&url, "a", true).await;
    let mut b = join(&url, &mut a, "b").await;
    for i in 0..50 {
        send(&mut a, json!({ "t": "sync_peer_send", "id": i, "to_device": "b", "data": "eA==" })).await;
        let ack = recv(&mut a).await;
        assert_eq!(ack["t"], "sync_peer_ack", "send {i}: {ack}");
        assert_eq!(recv(&mut b).await["t"], "sync_peer_msg");
    }
}

#[tokio::test]
async fn revoke_closes_and_blocks_the_device() {
    let url = start(Opts::default()).await;
    let mut a = hello(&url, "a", true).await;
    let mut b = join(&url, &mut a, "b").await;

    // A non-creator may only revoke itself.
    send(&mut b, json!({ "t": "sync_revoke", "device_id": "a" })).await;
    assert_eq!(recv(&mut b).await["code"], "forbidden");

    send(&mut a, json!({ "t": "sync_revoke", "device_id": "b" })).await;
    let ack = recv(&mut a).await;
    assert_eq!(ack["t"], "control");
    assert_eq!(ack["code"], "ok");

    let e = recv(&mut b).await;
    assert_eq!(e["code"], "device_revoked");
    assert_closed(&mut b).await;
    // A ticket does not let a revoked device back in.
    let (mut b3, r) = hello_ticket(&url, "b", "anything").await;
    assert_eq!(r["code"], "device_revoked");
    assert_closed(&mut b3).await;
    let (mut b2, r) = hello_raw(&url, "b", "tok-b", false).await;
    assert_eq!(r["code"], "device_revoked");
    assert_closed(&mut b2).await;

    // a still works and can bring in another device.
    let _c = join(&url, &mut a, "c").await;
    assert_eq!(peers(&mut a).await, vec!["c"]);
}

#[tokio::test]
async fn creator_leaving_moves_the_role_and_tells_the_group() {
    let url = start(Opts::default()).await;
    let mut a = hello(&url, "a", true).await;
    let mut b = join(&url, &mut a, "b").await;

    send(&mut a, json!({ "t": "sync_revoke", "device_id": "a", "successor": "b" })).await;
    let info = recv(&mut b).await;
    assert_eq!((info["t"].as_str(), info["creator_device"].as_str()), (Some("sync_group_info"), Some("b")), "got {info}");
    // The new creator may invite and remove others; the welcome reports it.
    let mut c = join(&url, &mut b, "c").await;
    send(&mut b, json!({ "t": "sync_revoke", "device_id": "c" })).await;
    assert_eq!(recv(&mut b).await["code"], "ok");
    assert_eq!(recv(&mut c).await["code"], "device_revoked");
    let (_b2, w) = hello_raw(&url, "b", "tok-b", false).await;
    assert_eq!(w["creator_device"], "b");
}

#[tokio::test]
async fn sync_and_remote_control_frames_do_not_mix() {
    let url = start(Opts::default()).await;

    // Sync frames before sync_hello.
    let (mut ws, _) = connect_async(&url).await.unwrap();
    send(&mut ws, json!({ "t": "sync_invite", "ticket_hash": sha_hex(b"t") })).await;
    assert_eq!(recv(&mut ws).await["code"], "forbidden");

    // Remote Control frames on a sync connection.
    let mut a = hello(&url, "a", true).await;
    send(&mut a, json!({ "t": "join", "pair_code": "x" })).await;
    assert_eq!(recv(&mut a).await["code"], "forbidden");
    send(&mut a, json!({ "t": "keepalive" })).await;
    assert_eq!(recv(&mut a).await["t"], "keepalive");

    // Sync frames on a registered Host.
    let (mut host, _) = connect_async(&url).await.unwrap();
    send(&mut host, json!({ "t": "register_host", "device_id": "h", "auth_token": TEST_TOKEN })).await;
    assert_eq!(recv(&mut host).await["t"], "control");
    send(
        &mut host,
        json!({ "t": "sync_hello", "auth_token": TEST_TOKEN, "group_id": GROUP, "device_id": "h",
                "device_token": "t", "create": true }),
    )
    .await;
    assert_eq!(recv(&mut host).await["code"], "forbidden");
}

async fn hello_group(url: &str, group: &str, device: &str) -> Ws {
    let (mut ws, _) = connect_async(url).await.expect("connect");
    send(
        &mut ws,
        json!({ "t": "sync_hello", "auth_token": TEST_TOKEN, "group_id": group,
                "device_id": device, "device_token": format!("tok-{device}"), "create": true }),
    )
    .await;
    assert_eq!(recv(&mut ws).await["t"], "sync_welcome");
    ws
}

async fn peers(ws: &mut Ws) -> Vec<String> {
    send(ws, json!({ "t": "sync_peers", "id": "p" })).await;
    let r = recv(ws).await;
    assert_eq!((r["t"].as_str(), r["id"].as_str()), (Some("sync_peers"), Some("p")), "got {r}");
    r["devices"].as_array().unwrap().iter().map(|d| d.as_str().unwrap().to_string()).collect()
}

#[tokio::test]
async fn peer_send_forwards_and_acks() {
    let url = start(Opts::default()).await;
    let mut a = hello(&url, "a", true).await;
    let mut b = join(&url, &mut a, "b").await;
    // A second, older connection of b: the most recent one gets the message.
    let mut b2 = hello(&url, "b", false).await;

    send(&mut a, json!({ "t": "sync_peer_send", "id": 9, "to_device": "b", "data": "c2VhbGVk" })).await;
    let ack = recv(&mut a).await;
    assert_eq!((ack["t"].as_str(), ack["id"].as_u64()), (Some("sync_peer_ack"), Some(9)), "got {ack}");
    let msg = recv(&mut b2).await;
    assert_eq!(msg["t"], "sync_peer_msg");
    assert_eq!(msg["from_device"], "a");
    assert_eq!(msg["data"], "c2VhbGVk");
    assert!(msg.get("id").is_none(), "no id on a forwarded message: {msg}");
    assert!(recv_within(&mut b, Duration::from_millis(300)).await.is_none());

    // And back.
    send(&mut b2, json!({ "t": "sync_peer_send", "id": "r", "to_device": "a", "data": "cmVwbHk=" })).await;
    assert_eq!(recv(&mut b2).await["t"], "sync_peer_ack");
    let msg = recv(&mut a).await;
    assert_eq!((msg["from_device"].as_str(), msg["data"].as_str()), (Some("b"), Some("cmVwbHk=")));

    // Missing fields.
    send(&mut a, json!({ "t": "sync_peer_send", "id": 1, "data": "eA==" })).await;
    let e = recv(&mut a).await;
    assert_eq!((e["code"].as_str(), e["id"].as_u64()), (Some("bad_request"), Some(1)));
    send(&mut a, json!({ "t": "sync_peer_send", "to_device": "b" })).await;
    assert_eq!(recv(&mut a).await["code"], "bad_request");
}

#[tokio::test]
async fn peer_send_to_offline_or_foreign_device_fails() {
    let url = start(Opts::default()).await;
    let mut a = hello(&url, "a", true).await;
    let b = join(&url, &mut a, "b").await;
    drop(b);
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Registered but not connected.
    send(&mut a, json!({ "t": "sync_peer_send", "id": 2, "to_device": "b", "data": "eA==" })).await;
    let e = recv(&mut a).await;
    assert_eq!((e["t"].as_str(), e["code"].as_str(), e["id"].as_u64()), (Some("error"), Some("peer_offline"), Some(2)));
    // Unknown device and self.
    for to in ["nobody", "a"] {
        send(&mut a, json!({ "t": "sync_peer_send", "to_device": to, "data": "eA==" })).await;
        assert_eq!(recv(&mut a).await["code"], "peer_offline", "to {to}");
    }
    // A live device of another group.
    let mut x = hello_group(&url, "grp-2", "x").await;
    send(&mut a, json!({ "t": "sync_peer_send", "to_device": "x", "data": "eA==" })).await;
    assert_eq!(recv(&mut a).await["code"], "peer_offline");
    assert!(recv_within(&mut x, Duration::from_millis(300)).await.is_none());
    // Same device id in another group is a different device.
    let mut b_other = hello_group(&url, "grp-3", "b").await;
    send(&mut a, json!({ "t": "sync_peer_send", "to_device": "b", "data": "eA==" })).await;
    assert_eq!(recv(&mut a).await["code"], "peer_offline");
    assert!(recv_within(&mut b_other, Duration::from_millis(300)).await.is_none());

    // Revoked device, even while still connected.
    let mut b = hello(&url, "b", false).await;
    send(&mut a, json!({ "t": "sync_revoke", "device_id": "b" })).await;
    assert_eq!(recv(&mut a).await["code"], "ok");
    send(&mut a, json!({ "t": "sync_peer_send", "to_device": "b", "data": "eA==" })).await;
    assert_eq!(recv(&mut a).await["code"], "peer_offline");
    assert_eq!(recv(&mut b).await["code"], "device_revoked");
}

#[tokio::test]
async fn sync_peers_lists_live_group_devices() {
    let url = start(Opts::default()).await;
    let mut a = hello(&url, "a", true).await;
    assert!(peers(&mut a).await.is_empty());
    let b = join(&url, &mut a, "b").await;
    let b2 = hello(&url, "b", false).await;
    let _a2 = hello(&url, "a", false).await;
    let _other = hello_group(&url, "grp-2", "z").await;
    assert_eq!(peers(&mut a).await, vec!["b"], "deduped, caller and other groups excluded");
    drop(b);
    drop(b2);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(peers(&mut a).await.is_empty());

    // Peer frames need a hello first.
    let (mut ws, _) = connect_async(&url).await.unwrap();
    send(&mut ws, json!({ "t": "sync_peers" })).await;
    assert_eq!(recv(&mut ws).await["code"], "forbidden");
    send(&mut ws, json!({ "t": "sync_peer_send", "to_device": "a", "data": "eA==" })).await;
    assert_eq!(recv(&mut ws).await["code"], "forbidden");
}

#[tokio::test]
async fn peer_send_too_large() {
    let url = start(Opts::default()).await;
    let mut a = hello(&url, "a", true).await;
    let mut b = join(&url, &mut a, "b").await;
    // Exactly the base64 of the max peer message passes; one more char does not.
    let max_chars = (4 * 1024 * 1024 + 64usize).div_ceil(3) * 4;
    let data = "A".repeat(max_chars);
    send(&mut a, json!({ "t": "sync_peer_send", "to_device": "b", "data": data })).await;
    assert_eq!(recv(&mut a).await["t"], "sync_peer_ack");
    assert_eq!(recv(&mut b).await["data"].as_str().map(str::len), Some(max_chars));
    let data = "A".repeat(max_chars + 1);
    send(&mut a, json!({ "t": "sync_peer_send", "id": 3, "to_device": "b", "data": data })).await;
    let e = recv(&mut a).await;
    assert_eq!((e["code"].as_str(), e["id"].as_u64()), (Some("too_large"), Some(3)));
    assert!(recv_within(&mut b, Duration::from_millis(300)).await.is_none());
}

/// Rows stored for `group` in each per-group table, and whether its row exists.
fn stored(db: &PathBuf, group: &str) -> (Vec<i64>, bool) {
    let c = rusqlite::Connection::open(db).unwrap();
    let counts = ["devices", "invites"]
        .iter()
        .map(|t| {
            c.query_row(&format!("SELECT COUNT(*) FROM {t} WHERE group_id = ?1"), [group], |r| r.get(0))
                .unwrap()
        })
        .collect();
    let exists = c
        .query_row("SELECT EXISTS (SELECT 1 FROM groups WHERE id = ?1)", [group], |r| r.get(0))
        .unwrap();
    (counts, exists)
}

fn file_bytes(db: &PathBuf) -> i64 {
    let c = rusqlite::Connection::open(db).unwrap();
    c.query_row("SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()", [], |r| r.get(0))
        .unwrap()
}

fn tables(db: &PathBuf) -> Vec<String> {
    let c = rusqlite::Connection::open(db).unwrap();
    let mut stmt = c.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name").unwrap();
    stmt.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
}

async fn revoke_self(ws: &mut Ws, device: &str) {
    send(ws, json!({ "t": "sync_revoke", "device_id": device })).await;
    assert_eq!(recv(ws).await["code"], "ok");
    assert_eq!(recv(ws).await["code"], "device_revoked");
}

#[tokio::test]
async fn last_device_leaving_purges_the_group() {
    let db = temp_db();
    let url = start(Opts { db: Some(db.clone()), ..Default::default() }).await;
    let mut a = hello(&url, "a", true).await;
    let mut b = join(&url, &mut a, "b").await;
    invite(&mut a, "unused").await;
    let mut other = hello_group(&url, "grp-2", "z").await;
    invite(&mut other, "unused").await;
    let other_before = stored(&db, "grp-2");

    // The creator revokes b, then leaves itself: nobody is left.
    send(&mut a, json!({ "t": "sync_revoke", "device_id": "b" })).await;
    assert_eq!(recv(&mut a).await["code"], "ok");
    assert_eq!(recv(&mut b).await["code"], "device_revoked");
    assert!(stored(&db, GROUP).1, "a is still active");
    revoke_self(&mut a, "a").await;

    assert_eq!(stored(&db, GROUP), (vec![0; 2], false), "devices, invites, group");
    assert_eq!(stored(&db, "grp-2"), other_before, "other groups are untouched");
    let (mut ws, r) = hello_raw(&url, "a", "tok-a", false).await;
    assert_eq!(r["code"], "group_not_found");
    assert_closed(&mut ws).await;
}

#[tokio::test]
async fn startup_purges_groups_without_an_active_device() {
    let db = temp_db();
    let url = start(Opts { db: Some(db.clone()), ..Default::default() }).await;
    let mut a = hello(&url, "a", true).await;
    let _b = join(&url, &mut a, "b").await;
    let _z = hello_group(&url, "grp-2", "z").await;
    let kept = stored(&db, "grp-2");
    // A group every device left before purging existed.
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute("UPDATE devices SET revoked = 1 WHERE group_id = ?1", [GROUP])
        .unwrap();
    assert!(stored(&db, GROUP).1);

    let url = start(Opts { db: Some(db.clone()), ..Default::default() }).await;
    assert_eq!(stored(&db, GROUP), (vec![0; 2], false));
    assert_eq!(stored(&db, "grp-2"), kept);
    let (_, r) = hello_raw(&url, "a", "tok-a", false).await;
    assert_eq!(r["code"], "group_not_found");
    let _z = hello_group(&url, "grp-2", "z").await;
}

/// A store written by the relay that kept ops and blobs: its data tables are
/// dropped at startup and the file shrinks; groups and devices keep working.
#[tokio::test]
async fn startup_drops_the_old_data_store() {
    let db = temp_db();
    {
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE groups (id TEXT PRIMARY KEY, created_at INTEGER NOT NULL, last_seq INTEGER NOT NULL DEFAULT 0,
                 bytes INTEGER NOT NULL DEFAULT 0, creator_device TEXT);
             CREATE TABLE devices (group_id TEXT NOT NULL REFERENCES groups(id), device_id TEXT NOT NULL, token_hash TEXT NOT NULL,
                 revoked INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL, PRIMARY KEY (group_id, device_id));
             CREATE TABLE ops (group_id TEXT NOT NULL REFERENCES groups(id), seq INTEGER NOT NULL, device_id TEXT NOT NULL,
                 key TEXT NOT NULL, blob TEXT NOT NULL, PRIMARY KEY (group_id, seq));
             CREATE INDEX idx_ops_key ON ops(group_id, key);
             CREATE TABLE blobs (group_id TEXT NOT NULL REFERENCES groups(id), sha TEXT NOT NULL, data BLOB NOT NULL,
                 device_id TEXT, released_at INTEGER, PRIMARY KEY (group_id, sha));
             CREATE INDEX idx_blobs_released ON blobs(group_id, released_at) WHERE released_at IS NOT NULL;
             CREATE TABLE store_epoch (id INTEGER PRIMARY KEY CHECK (id = 1), epoch TEXT NOT NULL);
             CREATE TABLE epochs (group_id TEXT NOT NULL REFERENCES groups(id), epoch TEXT NOT NULL, head INTEGER NOT NULL,
                 PRIMARY KEY (group_id, epoch));
             CREATE TABLE invites (group_id TEXT NOT NULL REFERENCES groups(id), ticket_hash TEXT NOT NULL,
                 expires_at INTEGER NOT NULL, used INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (group_id, ticket_hash));",
        )
        .unwrap();
        c.execute(
            "INSERT INTO groups (id, created_at, last_seq, bytes, creator_device) VALUES (?1, 0, 2, 0, 'a')",
            [GROUP],
        )
        .unwrap();
        c.execute(
            "INSERT INTO devices (group_id, device_id, token_hash, created_at) VALUES (?1, 'a', ?2, 0)",
            [GROUP, &sha_hex(b"tok-a")],
        )
        .unwrap();
        c.execute("INSERT INTO ops VALUES (?1, 1, 'a', 'k1', 'AA'), (?1, 2, 'a', 'k2', 'BB')", [GROUP])
            .unwrap();
        let data = vec![7u8; 2 * 1024 * 1024];
        c.execute(
            "INSERT INTO blobs (group_id, sha, data, device_id) VALUES (?1, 's', ?2, 'a')",
            rusqlite::params![GROUP, data],
        )
        .unwrap();
        c.execute("INSERT INTO store_epoch VALUES (1, 'e1')", []).unwrap();
        c.execute("INSERT INTO epochs VALUES (?1, 'e1', 2)", [GROUP]).unwrap();
    }
    let before = file_bytes(&db);

    let url = start(Opts { db: Some(db.clone()), ..Default::default() }).await;
    assert_eq!(tables(&db), vec!["devices", "groups", "invites"]);
    let after = file_bytes(&db);
    assert!(after < before - 1024 * 1024, "vacuumed: {before} -> {after}");

    let (mut a, w) = hello_raw(&url, "a", "tok-a", false).await;
    assert_eq!(w["t"], "sync_welcome", "got {w}");
    assert_eq!(w["creator_device"], "a");
    let mut b = join(&url, &mut a, "b").await;
    send(&mut b, json!({ "t": "sync_peer_send", "to_device": "a", "data": "eA==" })).await;
    assert_eq!(recv(&mut b).await["t"], "sync_peer_ack");
    assert_eq!(recv(&mut a).await["from_device"], "b");

    // A second start finds nothing to migrate.
    let _url = start(Opts { db: Some(db.clone()), ..Default::default() }).await;
    assert_eq!(tables(&db), vec!["devices", "groups", "invites"]);
}
