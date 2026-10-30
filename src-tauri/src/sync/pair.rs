//! Sync group membership: create, join (over the password-login channel),
//! leave (design §3 "Pairing").
//!
//! Group state lives in the Keychain (`secrets::SyncSecrets`): group id, the
//! 32-byte group key, this device's relay token and its sync device id.
//!
//! Join reuses Remote Control's password login: the new device dials the
//! relay as a Controller on the target host's login code, runs the same
//! X25519 handshake keyed by `PBKDF2(master password)` (a Rust port of
//! `src/controller/login.ts`), and sends a sealed `{kind:'cmd',
//! action:'sync_join'}` instead of the plain login proof. A host that is in a
//! group answers with a sealed `{kind:'sync_grant', group_id, group_key, ...}`
//! (see [`host_join_reply`], called from `remote::agent`'s login path). The
//! joining device then mints its own device token, which the relay registers
//! on first hello (TOFU).

use super::crypto::{generate_group_key, SyncKeys, KEY_LEN};
use super::transport::{ConnectParams, Connection, TransportError, CONNECT_TIMEOUT};
use crate::db::Db;
use crate::remote::protocol::{Envelope, FrameType, HandshakePayload};
use crate::secrets::SyncSecrets;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use remote_e2e::{host_fingerprint, Keypair, Session};
use serde_json::{json, Value};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

/// The sealed command a joining device sends on the login channel.
pub const SYNC_JOIN_ACTION: &str = "sync_join";
/// Same budget as the web controller's password login.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinError {
    /// Nobody listening on that login code (offline, no master password, wrong alias).
    HostOffline,
    WrongPassword,
    Network(String),
    /// The host is reachable but not in a sync group.
    HostNotInGroup,
    /// The host answered with a plain login grant: it predates sync.
    HostUnsupported,
    Other(String),
}

impl std::fmt::Display for JoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JoinError::HostOffline => f.write_str("host_offline: no device answered on that alias"),
            JoinError::WrongPassword => f.write_str("wrong_password"),
            JoinError::Network(e) => write!(f, "network: {e}"),
            JoinError::HostNotInGroup => f.write_str("host_not_in_group: that device has no sync group"),
            JoinError::HostUnsupported => f.write_str("host_unsupported: that device's Devdy does not support sync"),
            JoinError::Other(e) => f.write_str(e),
        }
    }
}

/// What a host hands a joining device.
#[derive(Clone)]
pub struct Grant {
    pub group_id: String,
    pub group_key: Vec<u8>,
    /// The host's relay token, for a joiner without one of its own.
    pub relay_token: Option<String>,
}

fn random_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    dryoc::rng::copy_randombytes(&mut b);
    hex::encode(b)
}

/// This machine's sync device id: sync-core's `sync_device` row when that
/// table exists, else the id already kept in the Keychain, else a new one.
pub async fn device_id(db: &Db) -> String {
    if let Ok(Some(id)) = sqlx::query_scalar::<_, String>("SELECT id FROM sync_device LIMIT 1")
        .fetch_optional(db)
        .await
    {
        if !id.is_empty() {
            return id;
        }
    }
    if let Some(s) = crate::secrets::get_sync_secrets() {
        return s.device_id;
    }
    uuid::Uuid::new_v4().to_string()
}

/// Relay URL + host token: Remote Control's config first, else the token the
/// group handed over at join.
async fn relay_endpoint(db: &Db, sync: Option<&SyncSecrets>) -> Result<(String, String), String> {
    let url = crate::remote::get_setting(db, crate::remote::KEY_RELAY_URL)
        .await
        .ok_or("relay URL is not configured (Settings → Remote Control)")?;
    let token = crate::secrets::get_remote_auth_token()
        .or_else(|| sync.and_then(|s| s.relay_token.clone()))
        .ok_or("relay token is not set (Settings → Remote Control)")?;
    Ok((url, token))
}

/// Connection parameters for a configured group.
pub async fn connect_params(db: &Db, sync: &SyncSecrets) -> Result<ConnectParams, String> {
    let (relay_url, auth_token) = relay_endpoint(db, Some(sync)).await?;
    Ok(ConnectParams {
        relay_url,
        auth_token,
        group_id: sync.group_id.clone(),
        device_id: sync.device_id.clone(),
        device_token: sync.device_token.clone(),
    })
}

pub fn keys_of(sync: &SyncSecrets) -> Result<SyncKeys, String> {
    let raw = hex::decode(&sync.group_key).map_err(|_| "stored group key is corrupt".to_string())?;
    SyncKeys::derive(&raw).map_err(|e| e.to_string())
}

/// Register on the relay with one hello and close.
async fn hello(params: &ConnectParams, create: bool) -> Result<(), String> {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    Connection::connect(params, create, tx).await.map(drop).map_err(|e| e.to_string())
}

/// First device: make a new group on the relay and store it.
pub async fn create_group(db: &Db) -> Result<SyncSecrets, String> {
    let (relay_url, auth_token) = relay_endpoint(db, None).await?;
    let sync = SyncSecrets {
        group_id: uuid::Uuid::new_v4().to_string(),
        group_key: hex::encode(generate_group_key()),
        device_token: random_hex(32),
        device_id: device_id(db).await,
        relay_token: None,
    };
    let params = ConnectParams {
        relay_url,
        auth_token,
        group_id: sync.group_id.clone(),
        device_id: sync.device_id.clone(),
        device_token: sync.device_token.clone(),
    };
    hello(&params, true).await?;
    crate::secrets::set_sync_secrets(&sync).map_err(|e| e.to_string())?;
    tracing::info!(event = "sync_group_created");
    Ok(sync)
}

/// Join the group of the device named `alias` using its master password.
pub async fn join(db: &Db, alias: &str, password: String) -> Result<SyncSecrets, String> {
    let relay_url = crate::remote::get_setting(db, crate::remote::KEY_RELAY_URL)
        .await
        .ok_or("relay URL is not configured (Settings → Remote Control)")?;
    let psk = tokio::task::spawn_blocking(move || crate::remote::login::derive_login_psk(&password))
        .await
        .map_err(|e| e.to_string())?;
    let code = crate::remote::login::login_code(Some(alias));
    let grant = request_grant(&relay_url, &code, &psk).await.map_err(|e| e.to_string())?;

    let sync = SyncSecrets {
        group_id: grant.group_id,
        group_key: hex::encode(&grant.group_key),
        device_token: random_hex(32),
        device_id: device_id(db).await,
        relay_token: grant.relay_token,
    };
    let params = connect_params(db, &sync).await?;
    hello(&params, false).await?;
    crate::secrets::set_sync_secrets(&sync).map_err(|e| e.to_string())?;
    tracing::info!(event = "sync_group_joined");
    Ok(sync)
}

/// Leave the group: revoke this device on the relay (best effort) and wipe
/// the Keychain entry. Returns whether the relay accepted the revoke.
pub async fn leave(db: &Db) -> Result<bool, String> {
    let Some(sync) = crate::secrets::get_sync_secrets() else {
        return Ok(false);
    };
    let revoked = match connect_params(db, &sync).await {
        Ok(params) => {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            match Connection::connect(&params, false, tx).await {
                Ok(conn) => conn.revoke(&sync.device_id).await.is_ok(),
                // Already revoked: nothing left to do on the relay.
                Err(TransportError::Rejected(_)) => true,
                Err(_) => false,
            }
        }
        Err(_) => false,
    };
    crate::secrets::delete_sync_secrets().map_err(|e| e.to_string())?;
    tracing::info!(event = "sync_group_left", revoked);
    Ok(revoked)
}

// ---- host side ---------------------------------------------------------------

/// Whether an opened login-channel frame asks for a sync grant.
pub fn is_sync_join(plaintext: &[u8]) -> bool {
    serde_json::from_slice::<Value>(plaintext)
        .ok()
        .is_some_and(|v| v["kind"] == "cmd" && v["action"] == SYNC_JOIN_ACTION)
}

/// The sealed-payload reply a host sends to a `sync_join` (password already
/// proven by the caller).
pub fn host_join_reply() -> Value {
    grant_payload(
        crate::secrets::get_sync_secrets().as_ref(),
        crate::secrets::get_remote_auth_token(),
    )
}

fn grant_payload(sync: Option<&SyncSecrets>, own_token: Option<String>) -> Value {
    match sync {
        Some(s) => json!({
            "kind": "sync_grant",
            "group_id": s.group_id,
            "group_key": s.group_key,
            "relay_token": own_token.or_else(|| s.relay_token.clone()),
        }),
        None => json!({ "kind": "sync_denied", "reason": "not_in_group" }),
    }
}

// ---- join client (Rust port of src/controller/login.ts) ----------------------

/// Run the password-login handshake on `login_code` and ask for a sync grant.
/// `psk` is the login PSK (`remote::login::derive_login_psk`).
pub async fn request_grant(relay_url: &str, login_code: &str, psk: &[u8; 32]) -> Result<Grant, JoinError> {
    let dial = tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(relay_url));
    let ws = match dial.await {
        Ok(Ok((ws, _))) => ws,
        Ok(Err(e)) => return Err(JoinError::Network(e.to_string())),
        Err(_) => return Err(JoinError::Network("connect timeout".into())),
    };
    let (mut sink, mut stream) = ws.split();
    let send = |v: Value| Message::Text(v.to_string());
    sink.send(send(json!({ "t": "join", "pair_code": login_code })))
        .await
        .map_err(|e| JoinError::Network(e.to_string()))?;

    let mut room_id: Option<String> = None;
    let mut session: Option<Session> = None;
    let mut seq = 0u64;
    let flow = async {
        while let Some(msg) = stream.next().await {
            let text = match msg {
                Ok(Message::Text(t)) => t,
                Ok(_) => continue,
                Err(_) => break,
            };
            let Ok(env) = serde_json::from_str::<Envelope>(&text) else { continue };
            match env.t {
                FrameType::RoomReady if !env.room_id.is_empty() => {
                    room_id = Some(env.room_id);
                    continue;
                }
                FrameType::Error
                    if matches!(env.code.as_deref(), Some("pair_invalid" | "room_not_found")) =>
                {
                    return Err(JoinError::HostOffline);
                }
                FrameType::PeerLeft => {
                    return Err(if session.is_some() { JoinError::WrongPassword } else { JoinError::HostOffline });
                }
                _ => {}
            }
            let Some(room) = room_id.clone().filter(|r| *r == env.room_id) else { continue };
            let Some(cipher) = env.cipher else { continue };
            match env.t {
                FrameType::Control if session.is_none() => {
                    let Some(json) = B64.decode(&cipher).ok() else { continue };
                    let Ok(HandshakePayload::HsOffer { host_pub, host_fingerprint: fp }) =
                        serde_json::from_slice::<HandshakePayload>(&json)
                    else {
                        continue;
                    };
                    let Some(host_pub) = crate::remote::pairing_unhex(&host_pub).filter(|b| b.len() == 32) else {
                        continue;
                    };
                    // No pinned fingerprint on this path (the password
                    // authenticates the host), but the offer must be self-consistent.
                    if host_fingerprint(&host_pub).ok().as_deref() != Some(fp.as_str()) {
                        continue;
                    }
                    let keypair = Keypair::generate();
                    let answer = HandshakePayload::HsAnswer {
                        controller_pub: crate::remote::pairing_hex(&keypair.public),
                    };
                    let answer = B64.encode(serde_json::to_vec(&answer).unwrap_or_default());
                    seq += 1;
                    sink.send(send(json!({ "t": "control", "room_id": room, "cipher": answer, "seq": seq })))
                        .await
                        .map_err(|e| JoinError::Network(e.to_string()))?;
                    let s = Session::new(psk, &keypair, &host_pub).map_err(|e| JoinError::Other(e.to_string()))?;
                    let cmd = json!({ "kind": "cmd", "action": SYNC_JOIN_ACTION }).to_string();
                    seq += 1;
                    sink.send(send(json!({ "t": "cmd", "room_id": room, "cipher": s.seal(cmd.as_bytes()), "seq": seq })))
                        .await
                        .map_err(|e| JoinError::Network(e.to_string()))?;
                    session = Some(s);
                }
                FrameType::Stream => {
                    let Some(s) = session.as_ref() else { continue };
                    let Ok(pt) = s.open(&cipher) else { continue };
                    let Ok(v) = serde_json::from_slice::<Value>(&pt) else { continue };
                    return match v["kind"].as_str() {
                        Some("sync_grant") => parse_grant(&v),
                        Some("sync_denied") => Err(JoinError::HostNotInGroup),
                        Some("login_granted") => Err(JoinError::HostUnsupported),
                        _ => continue,
                    };
                }
                _ => {}
            }
        }
        Err(JoinError::Network("connection closed".into()))
    };
    let result = match tokio::time::timeout(LOGIN_TIMEOUT, flow).await {
        Ok(r) => r,
        // Silence after our sealed frame = the host could not open it.
        Err(_) => Err(if session.is_some() { JoinError::WrongPassword } else { JoinError::HostOffline }),
    };
    let _ = sink.close().await;
    result
}

fn parse_grant(v: &Value) -> Result<Grant, JoinError> {
    let bad = || JoinError::Other("malformed sync grant".into());
    let group_id = v["group_id"].as_str().filter(|s| !s.is_empty()).ok_or_else(bad)?;
    let group_key = v["group_key"]
        .as_str()
        .and_then(|k| hex::decode(k).ok())
        .filter(|k| k.len() == KEY_LEN)
        .ok_or_else(bad)?;
    Ok(Grant {
        group_id: group_id.to_string(),
        group_key,
        relay_token: v["relay_token"].as_str().map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    fn secrets() -> SyncSecrets {
        SyncSecrets {
            group_id: "g-1".into(),
            group_key: hex::encode([5u8; 32]),
            device_token: "tok".into(),
            device_id: "dev-host".into(),
            relay_token: None,
        }
    }

    #[test]
    fn host_grant_carries_group_and_never_the_device_token() {
        let v = grant_payload(Some(&secrets()), Some("relay-tok".into()));
        assert_eq!(v["kind"], "sync_grant");
        assert_eq!(v["group_id"], "g-1");
        assert_eq!(v["relay_token"], "relay-tok");
        assert!(!v.to_string().contains("\"tok\""), "own device token must stay private");
        let g = parse_grant(&v).unwrap();
        assert_eq!(g.group_key, vec![5u8; 32]);

        assert_eq!(grant_payload(None, None)["kind"], "sync_denied");
    }

    #[test]
    fn recognises_the_join_command() {
        assert!(is_sync_join(br#"{"kind":"cmd","action":"sync_join"}"#));
        assert!(!is_sync_join(br#"{"kind":"cmd","action":"login"}"#));
        assert!(!is_sync_join(b"garbage"));
    }

    #[test]
    fn rejects_malformed_grants() {
        assert!(parse_grant(&json!({ "kind": "sync_grant", "group_id": "g", "group_key": "abcd" })).is_err());
        assert!(parse_grant(&json!({ "kind": "sync_grant", "group_key": hex::encode([1u8; 32]) })).is_err());
    }

    /// A fake relay + host on one socket, speaking the host side of the
    /// password-login flow exactly like `remote::agent` (offer on join, answer,
    /// open the first sealed cmd with the login psk, reply sealed).
    async fn fake_host(host_psk: [u8; 32], reply: Value) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let (mut sink, mut stream) = ws.split();
            let kp = Keypair::generate();
            let mut controller_pub = None;
            while let Some(Ok(Message::Text(t))) = stream.next().await {
                let v: Value = serde_json::from_str(&t).unwrap();
                let out = match v["t"].as_str().unwrap() {
                    "join" => {
                        assert_eq!(v["pair_code"], "devdy-login-v1/mini");
                        let offer = HandshakePayload::HsOffer {
                            host_pub: crate::remote::pairing_hex(&kp.public),
                            host_fingerprint: host_fingerprint(&kp.public).unwrap(),
                        };
                        let c = B64.encode(serde_json::to_vec(&offer).unwrap());
                        vec![
                            json!({ "t": "room_ready", "room_id": "rm1" }),
                            json!({ "t": "control", "room_id": "rm1", "cipher": c, "seq": 1 }),
                        ]
                    }
                    "control" => {
                        let raw = B64.decode(v["cipher"].as_str().unwrap()).unwrap();
                        if let HandshakePayload::HsAnswer { controller_pub: p } = serde_json::from_slice(&raw).unwrap() {
                            controller_pub = crate::remote::pairing_unhex(&p);
                        }
                        vec![]
                    }
                    "cmd" => {
                        let s = Session::new(&host_psk, &kp, controller_pub.as_ref().unwrap()).unwrap();
                        match s.open(v["cipher"].as_str().unwrap()) {
                            Ok(pt) => {
                                assert!(is_sync_join(&pt));
                                let c = s.seal(reply.to_string().as_bytes());
                                vec![json!({ "t": "stream", "room_id": "rm1", "cipher": c, "seq": 2 })]
                            }
                            // Wrong password: the host revokes the room.
                            Err(_) => vec![json!({ "t": "peer_left", "room_id": "rm1" })],
                        }
                    }
                    _ => vec![],
                };
                for o in out {
                    sink.send(Message::Text(o.to_string())).await.unwrap();
                }
            }
        });
        format!("ws://{addr}/ws")
    }

    #[tokio::test]
    async fn join_flow_receives_the_grant() {
        let psk = [9u8; 32];
        let reply = grant_payload(Some(&secrets()), Some("relay-tok".into()));
        let url = fake_host(psk, reply).await;
        let g = request_grant(&url, "devdy-login-v1/mini", &psk).await.unwrap();
        assert_eq!(g.group_id, "g-1");
        assert_eq!(g.group_key, vec![5u8; 32]);
        assert_eq!(g.relay_token.as_deref(), Some("relay-tok"));
    }

    #[tokio::test]
    async fn join_flow_wrong_password() {
        let url = fake_host([9u8; 32], json!({})).await;
        let err = request_grant(&url, "devdy-login-v1/mini", &[1u8; 32]).await.err();
        assert_eq!(err, Some(JoinError::WrongPassword));
    }

    #[tokio::test]
    async fn join_flow_host_without_group() {
        let psk = [9u8; 32];
        let url = fake_host(psk, grant_payload(None, None)).await;
        let err = request_grant(&url, "devdy-login-v1/mini", &psk).await.err();
        assert_eq!(err, Some(JoinError::HostNotInGroup));
    }
}
