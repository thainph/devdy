//! Device pull sync over the relay: the wire contract, the bundle key, chunk
//! reassembly, and the pulling side (device B).
//!
//! Who is online: every Devdy desktop of the owner holds the relay host token,
//! so B asks the relay with `list_hosts`. A Host that serves sync announces its
//! alias + app version on `register_host` (see `relay/src/presence.rs`).
//!
//! The pull runs on its own short-lived relay room, never the Remote Control
//! one (a code has one Controller; joining the session code would kick the
//! phone). Host A listens on [`sync_code`]`(device_id)` in the [`SYNC_SLOT`]
//! slot while Remote Control is on and a master password is set:
//!
//! 1. B joins the sync code → A sends `control` `hs_offer` (fresh X25519 key).
//! 2. B answers `control` `hs_answer`, then a sealed `cmd` [`SyncMsg::Request`]
//!    under `Session(psk = login PSK of the master password)`. The password
//!    never travels; only a peer knowing it can seal a frame A can open.
//! 3. A opens it (rate limited with password login, audited) or answers an
//!    unsealed `control` `sync_reject`. On success A sends sealed `stream`
//!    frames: [`SyncMsg::Start`] (salt, size, chunk count, SHA-256), then
//!    [`SyncMsg::Chunk`]s of at most [`CHUNK_BYTES`].
//! 4. B answers every chunk with a sealed `cmd` [`SyncMsg::Ack`]; A sends chunk
//!    `i` only once chunk `i - ACK_WINDOW` is acknowledged. The window is
//!    end-to-end flow control: the relay and A's shared outbound queue never
//!    hold more than [`ACK_WINDOW`] chunks of a pull, whatever B's link speed.
//! 5. B reassembles, checks size + SHA-256, closes the socket.
//!
//! The bundle itself is sealed by the bundle module under [`bundle_key`], a
//! second key derived from the same PSK and a per-pull host salt, so it stays
//! ciphertext even outside the E2E channel. The relay sees ciphertext only.

use std::time::Duration;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use hkdf::Hkdf;
use rand::RngCore;
use remote_e2e::{Keypair, Session};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::device_sync::bundle::SyncCategories;
use crate::remote::protocol::{Envelope, FrameType, HandshakePayload, HostPresence};
use crate::remote::{pairing_hex, pairing_unhex};

/// Rendezvous code prefix of the sync slot.
pub const SYNC_CODE: &str = "devdy-sync-v1";
/// Relay listen slot for device sync.
pub const SYNC_SLOT: &str = "sync";
/// Largest raw bundle bytes per `chunk` frame. Base64 + seal + base64 puts a
/// frame near 3.6 MiB; with [`ACK_WINDOW`] that bounds what a pull ever queues
/// ahead of the phone's frames on the shared socket to about 7 MiB.
pub const CHUNK_BYTES: usize = 2 * 1024 * 1024;
/// Largest bundle either side accepts.
pub const MAX_BUNDLE_BYTES: u64 = 256 * 1024 * 1024;
/// Sync wire version, carried in `request` and `start`. v2 added [`SyncMsg::Ack`]
/// and the puller's identity in the request.
pub const PROTOCOL_VERSION: u32 = 2;
/// Chunks the host may have sent that the puller has not acknowledged yet.
pub const ACK_WINDOW: u32 = 2;
/// Random bytes in the host salt and the puller nonce.
pub const RANDOM_LEN: usize = 32;
const BUNDLE_KEY_INFO: &[u8] = b"devdy-device-sync-bundle-v1";

// Stable error codes returned to the UI.
pub const ERR_WRONG_PASSWORD: &str = "wrong_password";
pub const ERR_DEVICE_OFFLINE: &str = "device_offline";
pub const ERR_RATE_LIMITED: &str = "rate_limited";
pub const ERR_BAD_REQUEST: &str = "bad_request";
pub const ERR_CHECKSUM: &str = "checksum_mismatch";
pub const ERR_TIMEOUT: &str = "timeout";
pub const ERR_DISCONNECTED: &str = "device_disconnected";
pub const ERR_PROTOCOL: &str = "protocol_error";
pub const ERR_SOURCE_FAILED: &str = "source_failed";
pub const ERR_TOO_LARGE: &str = "bundle_too_large";
pub const ERR_BUNDLE_CORRUPT: &str = "bundle_corrupt";
pub const ERR_BUNDLE_VERSION: &str = "bundle_unsupported_version";
pub const ERR_RELAY_UNREACHABLE: &str = "relay_unreachable";
pub const ERR_RELAY_AUTH: &str = "relay_auth_failed";
pub const ERR_RELAY_UNSUPPORTED: &str = "relay_unsupported";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Relay answers (`room_ready`, `hosts`) and the host's handshake offer.
const STEP_TIMEOUT: Duration = Duration::from_secs(15);
/// The host builds the bundle before `start`; skills with many files take a while.
const START_TIMEOUT: Duration = Duration::from_secs(180);
/// Longest silence between two chunks.
const CHUNK_TIMEOUT: Duration = Duration::from_secs(60);

/// The sync rendezvous code of a device. Keyed by the stable device id (not
/// the alias) so two hosts can never collide on one relay.
pub fn sync_code(device_id: &str) -> String {
    format!("{SYNC_CODE}/{device_id}")
}

/// Sealed payloads of the sync room, `{ "kind": ... }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SyncMsg {
    /// B → A (`cmd`): what to pull. `nonce` is B's fresh randomness (hex).
    Request {
        v: u32,
        categories: SyncCategories,
        nonce: String,
    },
    /// A → B (`stream`): the bundle that follows. `salt` hex, `sha256` hex of
    /// the bundle bytes.
    Start {
        v: u32,
        salt: String,
        total: u64,
        chunks: u32,
        sha256: String,
    },
    /// A → B (`stream`): one piece of the bundle, base64.
    Chunk { index: u32, data: String },
    /// A → B (`stream`): the pull was accepted but could not be served.
    Failed { reason: String },
    /// B → A (`cmd`): chunk `index` arrived; opens the window for one more.
    Ack { index: u32 },
}

/// Seal a [`SyncMsg`] into a data frame.
pub fn seal_msg(
    session: &Session,
    t: FrameType,
    room_id: &str,
    seq: u64,
    msg: &SyncMsg,
) -> Option<Envelope> {
    let json = serde_json::to_vec(msg).ok()?;
    Some(Envelope::data(t, room_id, session.seal(&json), seq))
}

/// Open a sealed [`SyncMsg`].
pub fn open_msg(session: &Session, cipher: &str) -> Option<SyncMsg> {
    serde_json::from_slice(&session.open(cipher).ok()?).ok()
}

/// An unsealed handshake/control payload as a `control` frame.
pub fn control_frame(room_id: &str, seq: u64, payload: &HandshakePayload) -> Option<Envelope> {
    let json = serde_json::to_vec(payload).ok()?;
    Some(Envelope::data(
        FrameType::Control,
        room_id,
        B64.encode(json),
        seq,
    ))
}

/// Decode an unsealed `control` payload.
pub fn parse_control(cipher: &str) -> Option<HandshakePayload> {
    serde_json::from_slice(&B64.decode(cipher).ok()?).ok()
}

pub fn random_bytes() -> [u8; RANDOM_LEN] {
    let mut b = [0u8; RANDOM_LEN];
    rand::thread_rng().fill_bytes(&mut b);
    b
}

/// The key the bundle is sealed with, derived by both sides:
/// `HKDF-SHA256(salt = host salt, ikm = psk, info = label || puller nonce ||
/// host pub || puller pub)`. `psk` is the master-password login PSK; the salt
/// and nonce make every pull's key fresh, the public keys bind it to this room.
pub fn bundle_key(
    psk: &[u8; 32],
    salt: &[u8],
    nonce: &[u8],
    host_pub: &[u8],
    puller_pub: &[u8],
) -> Result<[u8; 32], String> {
    if salt.len() != RANDOM_LEN || nonce.len() != RANDOM_LEN {
        return Err(ERR_PROTOCOL.to_string());
    }
    let mut info =
        Vec::with_capacity(BUNDLE_KEY_INFO.len() + nonce.len() + host_pub.len() + puller_pub.len());
    info.extend_from_slice(BUNDLE_KEY_INFO);
    info.extend_from_slice(nonce);
    info.extend_from_slice(host_pub);
    info.extend_from_slice(puller_pub);
    let mut key = [0u8; 32];
    Hkdf::<Sha256>::new(Some(salt), psk)
        .expand(&info, &mut key)
        .map_err(|_| ERR_PROTOCOL.to_string())?;
    Ok(key)
}

/// Number of chunks a bundle of `total` bytes is sent in.
pub fn chunk_count(total: u64) -> u32 {
    total.div_ceil(CHUNK_BYTES as u64) as u32
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    pairing_hex(&Sha256::digest(bytes))
}

/// Reassembles the chunks announced by a [`SyncMsg::Start`].
#[derive(Debug)]
pub struct Assembler {
    total: u64,
    chunks: u32,
    sha256: Vec<u8>,
    next: u32,
    buf: Vec<u8>,
}

impl Assembler {
    pub fn new(total: u64, chunks: u32, sha256_hex: &str) -> Result<Self, String> {
        if total > MAX_BUNDLE_BYTES {
            return Err(ERR_TOO_LARGE.to_string());
        }
        let sha256 = pairing_unhex(sha256_hex)
            .filter(|h| h.len() == 32)
            .ok_or_else(|| ERR_PROTOCOL.to_string())?;
        if chunks != chunk_count(total) {
            return Err(ERR_PROTOCOL.to_string());
        }
        Ok(Self {
            total,
            chunks,
            sha256,
            next: 0,
            // Grow with the data rather than trusting the header up front.
            buf: Vec::with_capacity(total.min(CHUNK_BYTES as u64 * 4) as usize),
        })
    }

    /// Append chunk `index`. Chunks must arrive in order, each at most
    /// [`CHUNK_BYTES`], never past the announced size.
    pub fn push(&mut self, index: u32, data: &[u8]) -> Result<(), String> {
        if index != self.next || index >= self.chunks || data.is_empty() || data.len() > CHUNK_BYTES
        {
            return Err(ERR_PROTOCOL.to_string());
        }
        if self.buf.len() as u64 + data.len() as u64 > self.total {
            return Err(ERR_PROTOCOL.to_string());
        }
        self.buf.extend_from_slice(data);
        self.next += 1;
        Ok(())
    }

    pub fn received(&self) -> u64 {
        self.buf.len() as u64
    }

    pub fn is_complete(&self) -> bool {
        self.next == self.chunks
    }

    /// The bundle, once every byte arrived and the SHA-256 matches.
    pub fn finish(self) -> Result<Vec<u8>, String> {
        if !self.is_complete() || self.buf.len() as u64 != self.total {
            return Err(ERR_PROTOCOL.to_string());
        }
        if Sha256::digest(&self.buf).as_slice() != self.sha256.as_slice() {
            return Err(ERR_CHECKSUM.to_string());
        }
        Ok(self.buf)
    }
}

/// An online device, as shown in the Sync dialog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OnlineDevice {
    pub device_id: String,
    pub alias: String,
    pub app_version: String,
}

/// Pull progress, emitted as `device-sync-progress`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Connecting,
    Auth,
    Downloading,
    Applying,
    Done,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Progress {
    pub phase: Phase,
    pub received: u64,
    pub total: u64,
}

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn dial(relay_url: &str) -> Result<Ws, String> {
    match tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(relay_url)).await {
        Ok(Ok((ws, _))) => Ok(ws),
        Ok(Err(e)) => {
            tracing::warn!(event = "device_sync_dial_failed", error = %e);
            Err(ERR_RELAY_UNREACHABLE.to_string())
        }
        Err(_) => Err(ERR_RELAY_UNREACHABLE.to_string()),
    }
}

async fn send(ws: &mut Ws, env: &Envelope) -> Result<(), String> {
    let text = serde_json::to_string(env).map_err(|_| ERR_PROTOCOL.to_string())?;
    ws.send(Message::Text(text))
        .await
        .map_err(|_| ERR_DISCONNECTED.to_string())
}

/// Next relay envelope within `wait`. Reading also answers the relay's pings.
async fn recv(ws: &mut Ws, wait: Duration) -> Result<Envelope, String> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let msg = match tokio::time::timeout_at(deadline, ws.next()).await {
            Err(_) => return Err(ERR_TIMEOUT.to_string()),
            Ok(None) | Ok(Some(Err(_))) => return Err(ERR_DISCONNECTED.to_string()),
            Ok(Some(Ok(m))) => m,
        };
        match msg {
            Message::Text(t) => match serde_json::from_str::<Envelope>(&t) {
                Ok(env) if env.t == FrameType::Keepalive => continue,
                Ok(env) => return Ok(env),
                // A frame type this build does not know: not for us.
                Err(_) => continue,
            },
            Message::Close(_) => return Err(ERR_DISCONNECTED.to_string()),
            _ => continue,
        }
    }
}

/// The other Devdy hosts online on the relay (this device excluded).
pub async fn list_devices(
    relay_url: &str,
    auth_token: &str,
    my_device_id: &str,
) -> Result<Vec<OnlineDevice>, String> {
    let mut ws = dial(relay_url).await?;
    send(&mut ws, &Envelope::list_hosts(auth_token, my_device_id)).await?;
    let result = loop {
        let env = recv(&mut ws, STEP_TIMEOUT).await?;
        match env.t {
            FrameType::Hosts => break Ok(devices_of(env.hosts.unwrap_or_default(), my_device_id)),
            FrameType::Error => {
                break Err(match env.code.as_deref() {
                    Some("auth_failed") => ERR_RELAY_AUTH,
                    // An older relay cannot parse `list_hosts`.
                    Some("bad_request") => ERR_RELAY_UNSUPPORTED,
                    Some("rate_limited") => ERR_RATE_LIMITED,
                    _ => ERR_PROTOCOL,
                }
                .to_string());
            }
            _ => continue,
        }
    };
    let _ = ws.close(None).await;
    result
}

fn devices_of(hosts: Vec<HostPresence>, my_device_id: &str) -> Vec<OnlineDevice> {
    hosts
        .into_iter()
        .filter(|h| !h.device_id.is_empty() && h.device_id != my_device_id)
        .map(|h| OnlineDevice {
            device_id: h.device_id,
            alias: h.alias,
            app_version: h.app_version,
        })
        .collect()
}

/// A pulled bundle and the key it is sealed with.
pub struct Pulled {
    pub bytes: Vec<u8>,
    pub key: [u8; 32],
}

/// Pull a bundle from `device_id`. `psk` is the login PSK of the master
/// password the user typed (derive it with `login::derive_login_psk` off the
/// async runtime). `progress` sees every phase up to `downloading`.
pub async fn pull(
    relay_url: &str,
    device_id: &str,
    categories: &SyncCategories,
    psk: &[u8; 32],
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<Pulled, String> {
    progress(Progress {
        phase: Phase::Connecting,
        received: 0,
        total: 0,
    });
    let mut ws = dial(relay_url).await?;
    let result = pull_on(&mut ws, device_id, categories, psk, progress).await;
    let _ = ws.close(None).await;
    result
}

async fn pull_on(
    ws: &mut Ws,
    device_id: &str,
    categories: &SyncCategories,
    psk: &[u8; 32],
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<Pulled, String> {
    send(ws, &Envelope::join(sync_code(device_id))).await?;
    let room_id = loop {
        let env = recv(ws, STEP_TIMEOUT).await?;
        match env.t {
            FrameType::RoomReady if !env.room_id.is_empty() => break env.room_id,
            FrameType::Error if env.code.as_deref() == Some("pair_invalid") => {
                return Err(ERR_DEVICE_OFFLINE.to_string())
            }
            FrameType::Error if env.code.as_deref() == Some("rate_limited") => {
                return Err(ERR_RATE_LIMITED.to_string())
            }
            FrameType::Error => return Err(ERR_PROTOCOL.to_string()),
            _ => continue,
        }
    };

    // The host's offer.
    let host_pub = loop {
        let env = room_frame(ws, &room_id, STEP_TIMEOUT).await?;
        if env.t != FrameType::Control {
            continue;
        }
        match env.cipher.as_deref().and_then(parse_control) {
            Some(HandshakePayload::HsOffer { host_pub, .. }) => {
                break pairing_unhex(&host_pub)
                    .filter(|b| b.len() == 32)
                    .ok_or_else(|| ERR_PROTOCOL.to_string())?
            }
            _ => continue,
        }
    };
    progress(Progress {
        phase: Phase::Auth,
        received: 0,
        total: 0,
    });
    let keypair = Keypair::generate();
    let session = Session::new(psk, &keypair, &host_pub).map_err(|_| ERR_PROTOCOL.to_string())?;
    let mut seq = 0u64;
    let mut next_seq = || {
        seq += 1;
        seq
    };
    let answer = HandshakePayload::HsAnswer {
        controller_pub: pairing_hex(&keypair.public),
    };
    send(
        ws,
        &control_frame(&room_id, next_seq(), &answer).ok_or_else(|| ERR_PROTOCOL.to_string())?,
    )
    .await?;
    let nonce = random_bytes();
    let request = SyncMsg::Request {
        v: PROTOCOL_VERSION,
        categories: categories.clone(),
        nonce: pairing_hex(&nonce),
    };
    let frame = seal_msg(&session, FrameType::Cmd, &room_id, next_seq(), &request)
        .ok_or_else(|| ERR_PROTOCOL.to_string())?;
    send(ws, &frame).await?;

    // Verdict: a reject, or the bundle header.
    let (salt, mut assembler) = loop {
        let env = room_frame(ws, &room_id, START_TIMEOUT).await?;
        match env.t {
            FrameType::Control => match env.cipher.as_deref().and_then(parse_control) {
                Some(HandshakePayload::SyncReject { reason }) => {
                    return Err(reject_code(&reason).to_string())
                }
                Some(HandshakePayload::HsReject) => return Err(ERR_WRONG_PASSWORD.to_string()),
                _ => continue,
            },
            FrameType::Stream => {
                let msg = env.cipher.as_deref().and_then(|c| open_msg(&session, c));
                match msg {
                    Some(SyncMsg::Start {
                        v,
                        salt,
                        total,
                        chunks,
                        sha256,
                    }) => {
                        if v != PROTOCOL_VERSION {
                            return Err(ERR_PROTOCOL.to_string());
                        }
                        let salt = pairing_unhex(&salt).ok_or_else(|| ERR_PROTOCOL.to_string())?;
                        break (salt, Assembler::new(total, chunks, &sha256)?);
                    }
                    Some(SyncMsg::Failed { .. }) => return Err(ERR_SOURCE_FAILED.to_string()),
                    // Only the host that knows the password can seal a frame
                    // that opens; anything else is not from it.
                    _ => return Err(ERR_PROTOCOL.to_string()),
                }
            }
            _ => continue,
        }
    };
    let total = assembler.total;
    progress(Progress {
        phase: Phase::Downloading,
        received: 0,
        total,
    });
    while !assembler.is_complete() {
        let env = room_frame(ws, &room_id, CHUNK_TIMEOUT).await?;
        if env.t != FrameType::Stream {
            continue;
        }
        match env.cipher.as_deref().and_then(|c| open_msg(&session, c)) {
            Some(SyncMsg::Chunk { index, data }) => {
                let data = B64.decode(data).map_err(|_| ERR_PROTOCOL.to_string())?;
                assembler.push(index, &data)?;
                let ack = seal_msg(
                    &session,
                    FrameType::Cmd,
                    &room_id,
                    next_seq(),
                    &SyncMsg::Ack { index },
                )
                .ok_or_else(|| ERR_PROTOCOL.to_string())?;
                send(ws, &ack).await?;
                progress(Progress {
                    phase: Phase::Downloading,
                    received: assembler.received(),
                    total,
                });
            }
            Some(SyncMsg::Failed { .. }) => return Err(ERR_SOURCE_FAILED.to_string()),
            _ => return Err(ERR_PROTOCOL.to_string()),
        }
    }
    let bytes = assembler.finish()?;
    let key = bundle_key(psk, &salt, &nonce, &host_pub, &keypair.public)?;
    Ok(Pulled { bytes, key })
}

/// Next frame of `room_id`; the room closing ends the pull.
async fn room_frame(ws: &mut Ws, room_id: &str, wait: Duration) -> Result<Envelope, String> {
    loop {
        let env = recv(ws, wait).await?;
        if env.room_id != room_id {
            continue;
        }
        match env.t {
            FrameType::PeerLeft => return Err(ERR_DISCONNECTED.to_string()),
            FrameType::Error => {
                return Err(match env.code.as_deref() {
                    Some("superseded") => ERR_DISCONNECTED,
                    _ => ERR_PROTOCOL,
                }
                .to_string())
            }
            _ => return Ok(env),
        }
    }
}

/// UI code for a host's `sync_reject` reason (unknown reasons stay generic).
fn reject_code(reason: &str) -> &'static str {
    match reason {
        ERR_WRONG_PASSWORD => ERR_WRONG_PASSWORD,
        ERR_RATE_LIMITED => ERR_RATE_LIMITED,
        _ => ERR_BAD_REQUEST,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 31 % 251) as u8).collect()
    }

    fn assemble(bytes: &[u8], sha: &str) -> Result<Vec<u8>, String> {
        let mut a = Assembler::new(bytes.len() as u64, chunk_count(bytes.len() as u64), sha)?;
        for (i, c) in bytes.chunks(CHUNK_BYTES).enumerate() {
            a.push(i as u32, c)?;
        }
        a.finish()
    }

    #[test]
    fn chunks_reassemble_to_the_same_bytes() {
        for len in [0, 1, CHUNK_BYTES, CHUNK_BYTES + 1, 2 * CHUNK_BYTES + 17] {
            let b = bundle(len);
            assert_eq!(assemble(&b, &sha256_hex(&b)).unwrap(), b, "len {len}");
        }
        assert_eq!(chunk_count(0), 0);
        assert_eq!(chunk_count(CHUNK_BYTES as u64), 1);
        assert_eq!(chunk_count(CHUNK_BYTES as u64 + 1), 2);
    }

    #[test]
    fn a_wrong_checksum_is_refused() {
        let b = bundle(CHUNK_BYTES + 5);
        let other = sha256_hex(b"something else");
        assert_eq!(assemble(&b, &other).unwrap_err(), ERR_CHECKSUM);
    }

    #[test]
    fn out_of_order_oversize_or_short_chunks_are_refused() {
        let b = bundle(2 * CHUNK_BYTES);
        let sha = sha256_hex(&b);
        let mut a = Assembler::new(b.len() as u64, 2, &sha).unwrap();
        assert!(a.push(1, &b[CHUNK_BYTES..]).is_err(), "out of order");
        assert!(
            a.push(0, &bundle(CHUNK_BYTES + 1)).is_err(),
            "chunk too big"
        );
        a.push(0, &b[..CHUNK_BYTES]).unwrap();
        assert!(a.push(0, &b[..CHUNK_BYTES]).is_err(), "duplicate");
        assert!(!a.is_complete());

        // Fewer bytes than announced.
        let mut short = Assembler::new(10, 1, &sha).unwrap();
        short.push(0, &[1, 2, 3]).unwrap();
        assert_eq!(short.finish().unwrap_err(), ERR_PROTOCOL);
        // More bytes than announced.
        let mut long = Assembler::new(3, 1, &sha).unwrap();
        assert!(long.push(0, &[1, 2, 3, 4]).is_err());
    }

    #[test]
    fn bad_headers_are_refused() {
        let sha = sha256_hex(b"");
        assert_eq!(
            Assembler::new(
                MAX_BUNDLE_BYTES + 1,
                chunk_count(MAX_BUNDLE_BYTES + 1),
                &sha
            )
            .unwrap_err(),
            ERR_TOO_LARGE
        );
        assert!(
            Assembler::new(10, 2, &sha).is_err(),
            "chunk count must match the size"
        );
        assert!(
            Assembler::new(10, 1, "abcd").is_err(),
            "sha must be 32 bytes"
        );
    }

    #[test]
    fn both_sides_derive_the_same_bundle_key() {
        let psk = [7u8; 32];
        let (salt, nonce) = (random_bytes(), random_bytes());
        let (host, puller) = (Keypair::generate(), Keypair::generate());
        let a = bundle_key(&psk, &salt, &nonce, &host.public, &puller.public).unwrap();
        let b = bundle_key(&psk, &salt, &nonce, &host.public, &puller.public).unwrap();
        assert_eq!(a, b);
        assert_ne!(
            a,
            bundle_key(&[8u8; 32], &salt, &nonce, &host.public, &puller.public).unwrap(),
            "password bound"
        );
        assert_ne!(
            a,
            bundle_key(&psk, &random_bytes(), &nonce, &host.public, &puller.public).unwrap(),
            "fresh per pull"
        );
        assert_ne!(
            a,
            bundle_key(&psk, &salt, &nonce, &puller.public, &host.public).unwrap(),
            "room bound"
        );
        assert!(bundle_key(&psk, &salt[..16], &nonce, &host.public, &puller.public).is_err());
    }

    #[test]
    fn sync_messages_round_trip() {
        let v = serde_json::to_value(SyncMsg::Chunk {
            index: 2,
            data: "QQ==".into(),
        })
        .unwrap();
        assert_eq!(v["kind"], "chunk");
        let reject = serde_json::to_value(HandshakePayload::SyncReject {
            reason: ERR_WRONG_PASSWORD.into(),
        })
        .unwrap();
        assert_eq!(
            reject,
            serde_json::json!({ "kind": "sync_reject", "reason": "wrong_password" })
        );
        let p = serde_json::to_value(Progress {
            phase: Phase::Downloading,
            received: 1,
            total: 2,
        })
        .unwrap();
        assert_eq!(
            p,
            serde_json::json!({ "phase": "downloading", "received": 1, "total": 2 })
        );
        let d = serde_json::to_value(OnlineDevice {
            device_id: "d".into(),
            alias: "a".into(),
            app_version: "1".into(),
        })
        .unwrap();
        assert_eq!(
            d,
            serde_json::json!({ "deviceId": "d", "alias": "a", "appVersion": "1" })
        );
    }

    #[test]
    fn the_listing_never_contains_this_device() {
        let h = |id: &str| HostPresence {
            device_id: id.into(),
            alias: format!("{id}-alias"),
            app_version: "1.0".into(),
        };
        let got = devices_of(vec![h("me"), h("other"), h("")], "me");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].device_id, "other");
        assert_eq!(got[0].alias, "other-alias");
    }

    #[test]
    fn sync_code_is_per_device() {
        assert_eq!(sync_code("abc"), "devdy-sync-v1/abc");
    }
}
