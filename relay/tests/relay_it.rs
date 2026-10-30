//! Integration tests for the relay using a real in-process WebSocket client
//! (`tokio-tungstenite`). The relay binds an ephemeral port (`127.0.0.1:0`) so
//! tests run without any external service or VPS.
//!
//! Model under test: a Host listens on a code for as long as its connection
//! lives; a Controller joins it (latest join wins); any dead connection —
//! closed or silent past the liveness timeout — closes its room and the
//! survivor gets `peer_left`.

use std::time::Duration;

use devdy_relay::config::Config;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

const TEST_TOKEN: &str = "test-secret-token-1234567890";

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

async fn start_relay() -> String {
    start_relay_with(Duration::from_secs(15), Duration::from_secs(45), 1000).await
}

async fn start_relay_with(ping_interval: Duration, peer_liveness_timeout: Duration, rate_limit_max: u32) -> String {
    let config = Config {
        bind: "127.0.0.1:0".to_string(),
        host_token: TEST_TOKEN.to_string(),
        rate_limit_max,
        rate_limit_window: Duration::from_secs(10),
        ping_interval,
        peer_liveness_timeout,
        max_rooms: 256,
        max_message_bytes: 32 * 1024 * 1024,
        sync_db: None,
        sync_max_group_bytes: 0,
        sync_max_blob_bytes: 0,
    };
    let (handle, accept_loop) = devdy_relay::server::serve(config)
        .await
        .expect("relay must bind");
    let addr = handle.local_addr;
    tokio::spawn(accept_loop);
    format!("ws://{addr}")
}

async fn connect(url: &str) -> Ws {
    let (ws, _resp) = connect_async(url).await.expect("client must connect");
    ws
}

async fn send(ws: &mut Ws, v: Value) {
    ws.send(Message::Text(v.to_string()))
        .await
        .expect("send must succeed");
}

/// Read the next JSON frame within `wait`; None on close/timeout. Reading also
/// answers the relay's pings, which keeps this socket alive.
async fn recv_within(ws: &mut Ws, wait: Duration) -> Option<Value> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let next = tokio::time::timeout_at(deadline, ws.next()).await;
        match next {
            Ok(Some(Ok(Message::Text(t)))) => return serde_json::from_str(&t).ok(),
            Ok(Some(Ok(Message::Close(_)))) => return None,
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(_))) | Ok(None) => return None,
            Err(_) => return None,
        }
    }
}

async fn recv_json(ws: &mut Ws) -> Option<Value> {
    recv_within(ws, Duration::from_secs(2)).await
}

/// Register a Host.
async fn register_host(url: &str) -> Ws {
    let mut ws = connect(url).await;
    send(
        &mut ws,
        json!({ "t": "register_host", "device_id": "dev-1", "auth_token": TEST_TOKEN }),
    )
    .await;
    let ack = recv_json(&mut ws).await.expect("register ack");
    assert_eq!(ack["t"], "control", "register should be acked");
    ws
}

/// Host listens on `code`; returns the room id the next join will get.
async fn listen(host: &mut Ws, code: &str) -> String {
    send(host, json!({ "t": "create_pair", "pair_code": code, "persistent": true })).await;
    let rr = recv_json(host).await.expect("ack for create_pair");
    assert_eq!(rr["t"], "room_ready");
    assert_eq!(rr["code"], "armed", "the ack is marked so the host ignores it");
    rr["room_id"].as_str().expect("room_id string").to_string()
}

/// Controller joins `code`; returns (controller socket, room id) after checking
/// both peers got `room_ready`.
async fn join(url: &str, host: &mut Ws, code: &str) -> (Ws, String) {
    let mut ctrl = connect(url).await;
    send(&mut ctrl, json!({ "t": "join", "pair_code": code })).await;
    let c = recv_json(&mut ctrl).await.expect("controller room_ready");
    assert_eq!(c["t"], "room_ready", "got {c}");
    let room = c["room_id"].as_str().unwrap().to_string();
    let h = recv_json(host).await.expect("host room_ready");
    assert_eq!(h["t"], "room_ready");
    assert_eq!(h["room_id"], room.as_str());
    assert!(h.get("code").is_none(), "a real join is not marked armed");
    (ctrl, room)
}

#[tokio::test]
async fn register_ok_and_bad_token_closes() {
    let url = start_relay().await;
    let _host = register_host(&url).await;

    let mut bad = connect(&url).await;
    send(&mut bad, json!({ "t": "register_host", "device_id": "x", "auth_token": "nope" })).await;
    let err = recv_json(&mut bad).await.expect("error frame");
    assert_eq!(err["code"], "auth_failed");
    assert!(recv_json(&mut bad).await.is_none(), "connection closed");
}

#[tokio::test]
async fn join_without_a_listening_host_is_invalid() {
    let url = start_relay().await;
    let mut ctrl = connect(&url).await;
    send(&mut ctrl, json!({ "t": "join", "pair_code": "nobody" })).await;
    let err = recv_json(&mut ctrl).await.expect("error");
    assert_eq!(err["code"], "pair_invalid");
}

#[tokio::test]
async fn join_uses_the_armed_room_id_and_routes_both_ways() {
    let url = start_relay().await;
    let mut host = register_host(&url).await;
    let armed = listen(&mut host, "code-1").await;
    let (mut ctrl, room) = join(&url, &mut host, "code-1").await;
    assert_eq!(room, armed);

    // Data routes immediately — no handshake_done gate.
    send(&mut ctrl, json!({ "t": "cmd", "room_id": room, "cipher": "AAA", "seq": 1 })).await;
    let got = recv_json(&mut host).await.expect("host gets cmd");
    assert_eq!(got["t"], "cmd");
    assert_eq!(got["cipher"], "AAA");

    send(&mut host, json!({ "t": "stream", "room_id": room, "cipher": "BBB", "seq": 1 })).await;
    let got = recv_json(&mut ctrl).await.expect("controller gets stream");
    assert_eq!(got["cipher"], "BBB");
}

#[tokio::test]
async fn data_to_an_unknown_room_is_refused() {
    let url = start_relay().await;
    let mut ctrl = connect(&url).await;
    send(&mut ctrl, json!({ "t": "cmd", "room_id": "rm_nope", "cipher": "x" })).await;
    let err = recv_json(&mut ctrl).await.expect("error");
    assert_eq!(err["code"], "room_not_found");
    assert_eq!(err["room_id"], "rm_nope");
}

#[tokio::test]
async fn newer_controller_supersedes_the_older_one() {
    let url = start_relay().await;
    let mut host = register_host(&url).await;
    listen(&mut host, "code-2").await;
    let (mut old, room_a) = join(&url, &mut host, "code-2").await;

    let mut new = connect(&url).await;
    send(&mut new, json!({ "t": "join", "pair_code": "code-2" })).await;

    let o = recv_json(&mut old).await.expect("old controller told");
    assert_eq!(o["t"], "error");
    assert_eq!(o["code"], "superseded");
    assert!(recv_json(&mut old).await.is_none(), "superseded socket is closed");
    let h1 = recv_json(&mut host).await.expect("host peer_left");
    assert_eq!(h1["t"], "peer_left");
    assert_eq!(h1["room_id"], room_a.as_str());
    let h2 = recv_json(&mut host).await.expect("host room_ready");
    assert_eq!(h2["t"], "room_ready");
    let n = recv_json(&mut new).await.expect("new room_ready");
    assert_eq!(n["room_id"], h2["room_id"]);
    assert_ne!(n["room_id"], room_a.as_str());
}

#[tokio::test]
async fn controller_drop_notifies_host_and_rejoin_works() {
    let url = start_relay().await;
    let mut host = register_host(&url).await;
    listen(&mut host, "code-3").await;
    let (ctrl, room) = join(&url, &mut host, "code-3").await;
    drop(ctrl);

    let pl = recv_json(&mut host).await.expect("peer_left");
    assert_eq!(pl["t"], "peer_left");
    assert_eq!(pl["room_id"], room.as_str());

    // The listen survives: a fresh controller joins right away.
    let (_ctrl2, room2) = join(&url, &mut host, "code-3").await;
    assert_ne!(room2, room);
}

#[tokio::test]
async fn host_drop_notifies_controller_and_relisten_restores_join() {
    let url = start_relay().await;
    let mut host = register_host(&url).await;
    listen(&mut host, "code-4").await;
    let (mut ctrl, _room) = join(&url, &mut host, "code-4").await;
    drop(host);

    let pl = recv_json(&mut ctrl).await.expect("peer_left");
    assert_eq!(pl["t"], "peer_left");

    send(&mut ctrl, json!({ "t": "join", "pair_code": "code-4" })).await;
    assert_eq!(recv_json(&mut ctrl).await.unwrap()["code"], "pair_invalid");

    // Host comes back on a new socket.
    let mut host2 = register_host(&url).await;
    listen(&mut host2, "code-4").await;
    let (_c, _r) = join(&url, &mut host2, "code-4").await;
}

#[tokio::test]
async fn new_host_socket_takes_over_a_stale_one() {
    let url = start_relay().await;
    let mut stale = register_host(&url).await;
    listen(&mut stale, "code-5").await;
    let (mut ctrl, _room) = join(&url, &mut stale, "code-5").await;

    // The host reconnected while its old socket is still open (half-open).
    let mut fresh = register_host(&url).await;
    listen(&mut fresh, "code-5").await;
    let pl = recv_json(&mut ctrl).await.expect("controller told to re-join");
    assert_eq!(pl["t"], "peer_left");

    send(&mut ctrl, json!({ "t": "join", "pair_code": "code-5" })).await;
    let rr = recv_json(&mut ctrl).await.unwrap();
    assert_eq!(rr["t"], "room_ready");
    let h = recv_json(&mut fresh).await.expect("the FRESH host gets the join");
    assert_eq!(h["room_id"], rr["room_id"]);
    drop(stale);
}

#[tokio::test]
async fn revoke_all_ends_the_session() {
    let url = start_relay().await;
    let mut host = register_host(&url).await;
    listen(&mut host, "code-6").await;
    let (mut ctrl, _room) = join(&url, &mut host, "code-6").await;

    send(&mut host, json!({ "t": "revoke", "room_id": "" })).await;
    assert_eq!(recv_json(&mut ctrl).await.unwrap()["t"], "peer_left");
    send(&mut ctrl, json!({ "t": "join", "pair_code": "code-6" })).await;
    assert_eq!(recv_json(&mut ctrl).await.unwrap()["code"], "pair_invalid");
}

#[tokio::test]
async fn listening_on_a_new_code_drops_the_old_session() {
    let url = start_relay().await;
    let mut host = register_host(&url).await;
    listen(&mut host, "old").await;
    let (mut ctrl, _room) = join(&url, &mut host, "old").await;

    listen(&mut host, "new").await;
    assert_eq!(recv_json(&mut ctrl).await.unwrap()["t"], "peer_left");
    send(&mut ctrl, json!({ "t": "join", "pair_code": "old" })).await;
    assert_eq!(recv_json(&mut ctrl).await.unwrap()["code"], "pair_invalid");
}

#[tokio::test]
async fn legacy_resume_is_refused() {
    let url = start_relay().await;
    let mut ctrl = connect(&url).await;
    send(&mut ctrl, json!({ "t": "resume", "resume_token": "rt_x" })).await;
    assert_eq!(recv_json(&mut ctrl).await.unwrap()["code"], "resume_invalid");
}

#[tokio::test]
async fn silent_controller_is_dropped_by_liveness() {
    let url = start_relay_with(Duration::from_millis(300), Duration::from_secs(2), 1000).await;
    let mut host = register_host(&url).await;
    listen(&mut host, "code-7").await;
    // The controller joins, then never reads or writes again — it never
    // answers pings, exactly like a half-open socket.
    let (ctrl, room) = join(&url, &mut host, "code-7").await;

    let pl = recv_within(&mut host, Duration::from_secs(5)).await.expect("peer_left");
    assert_eq!(pl["t"], "peer_left");
    assert_eq!(pl["room_id"], room.as_str());
    drop(ctrl);
}

#[tokio::test]
async fn a_peer_that_answers_pings_is_never_dropped() {
    let url = start_relay_with(Duration::from_millis(300), Duration::from_secs(1), 1000).await;
    let mut host = register_host(&url).await;
    listen(&mut host, "code-8").await;
    let (mut ctrl, _room) = join(&url, &mut host, "code-8").await;

    // Both sides only poll (answering pings) for well past the timeout.
    let (h, c) = tokio::join!(
        recv_within(&mut host, Duration::from_secs(3)),
        recv_within(&mut ctrl, Duration::from_secs(3)),
    );
    assert!(h.is_none() && c.is_none(), "no peer_left: {h:?} {c:?}");
}

#[tokio::test]
async fn data_frames_are_not_rate_limited() {
    let url = start_relay_with(Duration::from_secs(15), Duration::from_secs(45), 5).await;
    let mut host = register_host(&url).await;
    listen(&mut host, "code-9").await;
    let (mut ctrl, room) = join(&url, &mut host, "code-9").await;
    for i in 0..50 {
        send(&mut host, json!({ "t": "stream", "room_id": room, "cipher": "x", "seq": i })).await;
    }
    for _ in 0..50 {
        let f = recv_json(&mut ctrl).await.expect("every stream frame arrives");
        assert_eq!(f["t"], "stream");
    }
}

#[tokio::test]
async fn login_slot_is_tagged_and_survives_ending_the_session() {
    let url = start_relay().await;
    let mut host = register_host(&url).await;
    listen(&mut host, "session-code").await;
    send(&mut host, json!({ "t": "create_pair", "pair_code": "devdy-login", "slot": "login" })).await;
    let ack = recv_json(&mut host).await.unwrap();
    assert_eq!(ack["code"], "armed");
    assert_eq!(ack["slot"], "login");

    let mut ctrl = connect(&url).await;
    send(&mut ctrl, json!({ "t": "join", "pair_code": "devdy-login" })).await;
    assert_eq!(recv_json(&mut ctrl).await.unwrap()["t"], "room_ready");
    let h = recv_json(&mut host).await.unwrap();
    assert_eq!(h["t"], "room_ready");
    assert_eq!(h["slot"], "login", "the host learns which slot was joined");

    // Ending the session must not drop the login listen.
    send(&mut host, json!({ "t": "revoke", "room_id": "" })).await;
    let mut c2 = connect(&url).await;
    send(&mut c2, json!({ "t": "join", "pair_code": "devdy-login" })).await;
    assert_eq!(recv_json(&mut c2).await.unwrap()["t"], "room_ready");
}
