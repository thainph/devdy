//! Integration tests for the two-way sync store (design `two-way-sync` §9.2)
//! over a real in-process WebSocket, each relay with its own temp SQLite file.

use std::path::PathBuf;
use std::time::Duration;

use base64::Engine as _;
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
    max_group_bytes: u64,
    rate_limit_max: u32,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            sync: true,
            max_group_bytes: 2 * 1024 * 1024 * 1024,
            rate_limit_max: 1000,
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
        sync_db: opts.sync.then(temp_db),
        sync_max_group_bytes: opts.max_group_bytes,
        sync_max_blob_bytes: 4 * 1024 * 1024 + 64,
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

/// Join `device` to the group with a ticket that `member` registers.
async fn join(url: &str, member: &mut Ws, device: &str) -> Ws {
    let ticket = format!("ticket-for-{device}");
    send(member, json!({ "t": "sync_invite", "ticket_hash": sha_hex(ticket.as_bytes()), "ttl_secs": 60 })).await;
    let ack = recv(member).await;
    assert_eq!((ack["t"].as_str(), ack["code"].as_str()), (Some("control"), Some("ok")), "got {ack}");
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

async fn push(ws: &mut Ws, items: Value) -> Value {
    send(ws, json!({ "t": "sync_push", "items": items })).await;
    recv(ws).await
}

async fn pull(ws: &mut Ws, after: u64, limit: u64) -> Value {
    send(ws, json!({ "t": "sync_pull", "after_seq": after, "limit": limit })).await;
    let r = recv(ws).await;
    assert_eq!(r["t"], "sync_ops", "got {r}");
    r
}

fn seqs(page: &Value) -> Vec<u64> {
    page["items"].as_array().unwrap().iter().map(|i| i["seq"].as_u64().unwrap()).collect()
}

fn sha_hex(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

#[tokio::test]
async fn sync_disabled_without_db() {
    let url = start(Opts { sync: false, ..Default::default() }).await;
    let (mut ws, reply) = hello_raw(&url, "a", "t", true).await;
    assert_eq!(reply["t"], "error");
    assert_eq!(reply["code"], "sync_disabled");
    assert_closed(&mut ws).await;

    let (mut ws, _) = connect_async(&url).await.unwrap();
    send(&mut ws, json!({ "t": "sync_pull", "after_seq": 0 })).await;
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

    let mut a = hello(&url, "a", true).await;
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
async fn welcome_carries_last_seq() {
    let url = start(Opts::default()).await;
    let mut a = hello(&url, "a", true).await;
    push(&mut a, json!([{ "key": "k1", "blob": "AA" }, { "key": "k2", "blob": "BB" }])).await;
    let ticket = "t-b";
    send(&mut a, json!({ "t": "sync_invite", "ticket_hash": sha_hex(ticket.as_bytes()), "ttl_secs": 60 })).await;
    assert_eq!(recv(&mut a).await["code"], "ok");
    let (_, r) = hello_ticket(&url, "b", ticket).await;
    assert_eq!(r["t"], "sync_welcome");
    assert_eq!(r["last_seq"], 2);
}

#[tokio::test]
async fn push_ack_notify_pull() {
    let url = start(Opts::default()).await;
    let mut a = hello(&url, "a", true).await;
    let mut b = join(&url, &mut a, "b").await;

    send(
        &mut a,
        json!({ "t": "sync_push", "id": 7, "items": [{ "key": "k1", "blob": "b3AxCg==" }, { "key": "k2", "blob": "b3AyCg==" }] }),
    )
    .await;
    let ack = recv(&mut a).await;
    assert_eq!(ack["t"], "sync_ack");
    assert_eq!(ack["last_seq"], 2);
    assert_eq!(ack["id"], 7, "request id is echoed");

    let n = recv(&mut b).await;
    assert_eq!(n["t"], "sync_notify");
    assert_eq!(n["last_seq"], 2);
    // The pusher is not notified.
    assert!(recv_within(&mut a, Duration::from_millis(300)).await.is_none());

    let page = pull(&mut b, 0, 100).await;
    assert_eq!(seqs(&page), vec![1, 2]);
    assert_eq!(page["items"][0]["device_id"], "a");
    assert_eq!(page["items"][0]["blob"], "b3AxCg==");
    assert_eq!(page["items"][0]["key"], "k1");
    assert_eq!(page["last_seq"], 2);
    assert_eq!(page["more"], false);

    let page = pull(&mut b, 2, 100).await;
    assert!(seqs(&page).is_empty());
    assert_eq!(page["last_seq"], 2);

    // Empty push: ack with the head, no notify.
    let ack = push(&mut b, json!([])).await;
    assert_eq!(ack["last_seq"], 2);
    assert!(recv_within(&mut a, Duration::from_millis(300)).await.is_none());
}

#[tokio::test]
async fn push_supersedes_by_key() {
    let url = start(Opts::default()).await;
    let mut a = hello(&url, "a", true).await;
    push(&mut a, json!([{ "key": "k1", "blob": "v1" }, { "key": "k2", "blob": "v1" }])).await;
    let ack = push(&mut a, json!([{ "key": "k1", "blob": "v2" }])).await;
    assert_eq!(ack["last_seq"], 3);
    let page = pull(&mut a, 0, 0).await;
    assert_eq!(seqs(&page), vec![2, 3]);
    assert_eq!(page["items"][1]["blob"], "v2");
    assert_eq!(page["last_seq"], 3);
}

/// Finding #1: a device that was offline pushes an older edit of a row after
/// another device pushed a newer one. The relay must keep the newer op until
/// the stale pusher has pulled it.
#[tokio::test]
async fn stale_push_does_not_erase_an_unseen_op() {
    let url = start(Opts::default()).await;
    let mut a = hello(&url, "a", true).await;
    let mut b = join(&url, &mut a, "b").await;
    // A pushes t=200 at seq 1; B has only pulled up to 0.
    assert_eq!(push_base(&mut a, 0, json!([{ "key": "row", "blob": "t200" }])).await["last_seq"], 1);
    let _ = recv(&mut b).await; // notify
    assert_eq!(push_base(&mut b, 0, json!([{ "key": "row", "blob": "t100" }])).await["last_seq"], 2);
    assert_eq!(recv(&mut a).await["t"], "sync_notify");
    let page = pull(&mut a, 0, 0).await;
    let blobs: Vec<&str> = page["items"].as_array().unwrap().iter().map(|i| i["blob"].as_str().unwrap()).collect();
    assert_eq!(blobs, vec!["t200", "t100"], "both kept; devices resolve by HLC");
    // A has seen seq 2 now; its next edit replaces both.
    push_base(&mut a, 2, json!([{ "key": "row", "blob": "t300" }])).await;
    assert_eq!(recv(&mut b).await["t"], "sync_notify");
    assert_eq!(seqs(&pull(&mut b, 0, 0).await), vec![3]);
}

async fn push_base(ws: &mut Ws, base: u64, items: Value) -> Value {
    send(ws, json!({ "t": "sync_push", "base_seq": base, "items": items })).await;
    recv(ws).await
}

#[tokio::test]
async fn pull_is_paginated() {
    let url = start(Opts::default()).await;
    let mut a = hello(&url, "a", true).await;
    let items: Vec<Value> = (0..7).map(|i| json!({ "key": format!("k{i}"), "blob": "x" })).collect();
    push(&mut a, Value::Array(items)).await;

    let mut cursor = 0;
    let mut got = Vec::new();
    loop {
        let page = pull(&mut a, cursor, 3).await;
        got.extend(seqs(&page));
        cursor = page["last_seq"].as_u64().unwrap();
        if page["more"] == false {
            break;
        }
        assert_eq!(seqs(&page).len(), 3);
    }
    assert_eq!(got, (1..=7).collect::<Vec<_>>());
    assert_eq!(cursor, 7);

    // `limit` is capped at 500.
    let items: Vec<Value> = (0..600).map(|i| json!({ "key": format!("m{i}"), "blob": "y" })).collect();
    push(&mut a, Value::Array(items)).await;
    let page = pull(&mut a, 0, 100_000).await;
    assert_eq!(seqs(&page).len(), 500);
    assert_eq!(page["more"], true);
    assert_eq!(page["last_seq"], 500);
}

#[tokio::test]
async fn pull_page_is_byte_capped() {
    let url = start(Opts::default()).await;
    let mut a = hello(&url, "a", true).await;
    let big = "z".repeat(3 * 1024 * 1024);
    let items: Vec<Value> = (0..3).map(|i| json!({ "key": format!("k{i}"), "blob": big })).collect();
    assert_eq!(push(&mut a, Value::Array(items)).await["last_seq"], 3);

    let page = pull(&mut a, 0, 0).await;
    assert_eq!(seqs(&page), vec![1, 2], "8 MiB page budget fits two 3 MiB ops");
    assert_eq!(page["more"], true);
    let page = pull(&mut a, page["last_seq"].as_u64().unwrap(), 0).await;
    assert_eq!(seqs(&page), vec![3]);
    assert_eq!(page["more"], false);
}

#[tokio::test]
async fn data_frames_are_not_rate_limited() {
    let url = start(Opts { rate_limit_max: 3, ..Default::default() }).await;
    let mut a = hello(&url, "a", true).await;
    for i in 0..50 {
        let ack = push(&mut a, json!([{ "key": format!("k{i}"), "blob": "x" }])).await;
        assert_eq!(ack["t"], "sync_ack", "push {i}: {ack}");
    }
}

#[tokio::test]
async fn blob_put_get() {
    let url = start(Opts::default()).await;
    let mut a = hello(&url, "a", true).await;
    let mut b = join(&url, &mut a, "b").await;

    // A full 4 MiB sealed chunk fits in one frame.
    let data: Vec<u8> = (0..4 * 1024 * 1024 + 64).map(|i| (i % 251) as u8).collect();
    let sha = sha_hex(&data);
    send(&mut a, json!({ "t": "blob_put", "sha": sha, "data": b64(&data) })).await;
    let ack = recv(&mut a).await;
    assert_eq!(ack["t"], "blob_ack", "got {ack}");
    assert_eq!(ack["sha"], sha.as_str());
    // Idempotent.
    send(&mut a, json!({ "t": "blob_put", "sha": sha, "data": b64(&data) })).await;
    assert_eq!(recv(&mut a).await["t"], "blob_ack");

    send(&mut b, json!({ "t": "blob_get", "sha": sha })).await;
    let got = recv(&mut b).await;
    assert_eq!(got["t"], "blob_data");
    assert_eq!(got["sha"], sha.as_str());
    assert_eq!(got["data"], b64(&data));

    // sha mismatch.
    send(&mut a, json!({ "t": "blob_put", "sha": sha_hex(b"other"), "data": b64(b"bytes") })).await;
    assert_eq!(recv(&mut a).await["code"], "sha_mismatch");
    // Too large.
    let huge = vec![1u8; 4 * 1024 * 1024 + 65];
    send(&mut a, json!({ "t": "blob_put", "sha": sha_hex(&huge), "data": b64(&huge) })).await;
    assert_eq!(recv(&mut a).await["code"], "too_large");
    // Unknown sha.
    send(&mut b, json!({ "t": "blob_get", "sha": sha_hex(b"missing") })).await;
    assert_eq!(recv(&mut b).await["code"], "blob_not_found");
}

#[tokio::test]
async fn blob_release_is_acked_and_uploader_only() {
    let url = start(Opts::default()).await;
    let mut a = hello(&url, "a", true).await;
    let mut b = join(&url, &mut a, "b").await;
    let sha = sha_hex(b"tail");
    send(&mut a, json!({ "t": "blob_put", "sha": sha, "data": b64(b"tail") })).await;
    assert_eq!(recv(&mut a).await["t"], "blob_ack");

    // Another device can't release it.
    send(&mut b, json!({ "t": "blob_release", "id": 1, "shas": [sha] })).await;
    let r = recv(&mut b).await;
    assert_eq!((r["t"].as_str(), r["released"].as_u64(), r["id"].as_u64()), (Some("blob_released"), Some(0), Some(1)));
    // The uploader can; it stays fetchable during the grace period.
    send(&mut a, json!({ "t": "blob_release", "shas": [sha, "x"] })).await;
    assert_eq!(recv(&mut a).await["released"], 1);
    send(&mut b, json!({ "t": "blob_get", "sha": sha })).await;
    assert_eq!(recv(&mut b).await["t"], "blob_data");
    // Malformed.
    send(&mut a, json!({ "t": "blob_release" })).await;
    assert_eq!(recv(&mut a).await["t"], "error");
}

#[tokio::test]
async fn group_quota_is_enforced() {
    let url = start(Opts { max_group_bytes: 100, ..Default::default() }).await;
    let mut a = hello(&url, "a", true).await;
    let blob60 = "a".repeat(60);
    assert_eq!(push(&mut a, json!([{ "key": "k1", "blob": blob60 }])).await["t"], "sync_ack");
    // Superseding the same key frees its bytes.
    assert_eq!(push(&mut a, json!([{ "key": "k1", "blob": blob60 }])).await["t"], "sync_ack");
    let r = push(&mut a, json!([{ "key": "k2", "blob": blob60 }])).await;
    assert_eq!(r["code"], "quota_exceeded");

    let data = vec![7u8; 50];
    send(&mut a, json!({ "t": "blob_put", "sha": sha_hex(&data), "data": b64(&data) })).await;
    assert_eq!(recv(&mut a).await["code"], "quota_exceeded");
    let data = vec![7u8; 40];
    send(&mut a, json!({ "t": "blob_put", "sha": sha_hex(&data), "data": b64(&data) })).await;
    assert_eq!(recv(&mut a).await["t"], "blob_ack");

    // The rejected push stored nothing.
    let page = pull(&mut a, 0, 0).await;
    assert_eq!(seqs(&page), vec![2]);
}

#[tokio::test]
async fn revoke_closes_and_blocks_the_device() {
    let url = start(Opts::default()).await;
    let mut a = hello(&url, "a", true).await;
    let mut b = join(&url, &mut a, "b").await;

    send(&mut a, json!({ "t": "sync_revoke", "device_id": "b" })).await;
    let ack = recv(&mut a).await;
    assert_eq!(ack["t"], "control");
    assert_eq!(ack["code"], "ok");

    let e = recv(&mut b).await;
    assert_eq!(e["code"], "device_revoked");
    // A ticket does not let a revoked device back in.
    let (mut b3, r) = hello_ticket(&url, "b", "anything").await;
    assert_eq!(r["code"], "device_revoked");
    assert_closed(&mut b3).await;
    assert_closed(&mut b).await;

    let (mut b2, r) = hello_raw(&url, "b", "tok-b", false).await;
    assert_eq!(r["code"], "device_revoked");
    assert_closed(&mut b2).await;

    // The revoked device no longer gets notifications; a still works.
    assert_eq!(push(&mut a, json!([{ "key": "k", "blob": "x" }])).await["t"], "sync_ack");

    // A non-creator may only revoke itself.
    let mut c = join(&url, &mut a, "c").await;
    let mut d = join(&url, &mut a, "d").await;
    send(&mut c, json!({ "t": "sync_revoke", "device_id": "d" })).await;
    assert_eq!(recv(&mut c).await["code"], "forbidden");
    send(&mut c, json!({ "t": "sync_revoke", "device_id": "c" })).await;
    let mut codes = Vec::new();
    while let Some(f) = recv_within(&mut c, Duration::from_secs(2)).await {
        codes.push(f["code"].as_str().unwrap_or_default().to_string());
    }
    assert!(codes.contains(&"ok".to_string()) || codes.contains(&"device_revoked".to_string()), "{codes:?}");
    assert_eq!(push(&mut d, json!([{ "key": "k2", "blob": "y" }])).await["t"], "sync_ack");
}

#[tokio::test]
async fn creator_leaving_moves_the_role_and_tells_the_group() {
    let url = start(Opts::default()).await;
    let (mut a, w) = hello_raw(&url, "a", "tok-a", true).await;
    assert_eq!(w["creator_device"], "a");
    let mut b = join(&url, &mut a, "b").await;
    let mut c = join(&url, &mut a, "c").await;

    send(&mut a, json!({ "t": "sync_revoke", "device_id": "a", "successor": "c" })).await;
    for ws in [&mut b, &mut c] {
        let info = recv(ws).await;
        assert_eq!((info["t"].as_str(), info["creator_device"].as_str()), (Some("sync_group_info"), Some("c")), "got {info}");
    }
    // The new creator may remove others; the welcome reports it.
    send(&mut c, json!({ "t": "sync_revoke", "device_id": "b" })).await;
    assert_eq!(recv(&mut c).await["code"], "ok");
    let (_c2, w) = hello_raw(&url, "c", "tok-c", false).await;
    assert_eq!(w["creator_device"], "c");
}

#[tokio::test]
async fn sync_and_remote_control_frames_do_not_mix() {
    let url = start(Opts::default()).await;

    // Sync frames before sync_hello.
    let (mut ws, _) = connect_async(&url).await.unwrap();
    send(&mut ws, json!({ "t": "sync_push", "items": [] })).await;
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

    // Revoked device.
    let mut c = join(&url, &mut a, "c").await;
    send(&mut a, json!({ "t": "sync_revoke", "device_id": "c" })).await;
    assert_eq!(recv(&mut a).await["code"], "ok");
    send(&mut a, json!({ "t": "sync_peer_send", "to_device": "c", "data": "eA==" })).await;
    assert_eq!(recv(&mut a).await["code"], "peer_offline");
    assert_eq!(recv(&mut c).await["code"], "device_revoked");
}

#[tokio::test]
async fn sync_peers_lists_live_group_devices() {
    let url = start(Opts::default()).await;
    let mut a = hello(&url, "a", true).await;
    assert!(peers(&mut a).await.is_empty());
    let b = join(&url, &mut a, "b").await;
    let _b2 = hello(&url, "b", false).await;
    let _c = join(&url, &mut a, "c").await;
    let _a2 = hello(&url, "a", false).await;
    let _other = hello_group(&url, "grp-2", "z").await;
    assert_eq!(peers(&mut a).await, vec!["b", "c"], "deduped, caller and other groups excluded");
    drop(b);
    drop(_b2);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(peers(&mut a).await, vec!["c"]);

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
    // Exactly the base64 of the max blob passes; one more char does not.
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
