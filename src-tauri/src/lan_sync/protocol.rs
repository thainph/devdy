//! Wire protocol of the LAN transfer.
//!
//! Plain TCP, every frame is `len(u32 BE) | bytes`. The handshake is a SPAKE2 exchange keyed by
//! the one-time code shown on the sharing device, so the code never crosses the wire and an
//! eavesdropper or a man in the middle learns nothing it could brute-force offline:
//!
//! 1. client → server  `Hello { proto, deviceName, pake }`            (JSON)
//! 2. server → client  `Welcome { proto, deviceName, pake }` / `Refused { reason }`
//! 3. client → server  sealed `{"op":"pull"}` — key confirmation: the server sends nothing sealed
//!    before this opens, so a wrong code costs the client exactly one online guess
//! 4. server → client  sealed transfer payload, or an error frame
//! 5. client → server  sealed `{"op":"ack"}`
//!
//! Post-handshake frames start with a tag byte: `SEALED` (`nonce | XChaCha20-Poly1305 ct`, one
//! key per direction) or `ERROR` (an ASCII reason code, see [`reason`]).

use dryoc::classic::crypto_aead_xchacha20poly1305_ietf::{
    crypto_aead_xchacha20poly1305_ietf_decrypt, crypto_aead_xchacha20poly1305_ietf_encrypt, Key,
    Nonce,
};
use dryoc::classic::crypto_generichash::crypto_generichash;
use dryoc::rng::copy_randombytes;
use serde::{Deserialize, Serialize};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const PROTO_VERSION: u32 = 1;
/// mDNS / Bonjour service type the sharing device advertises.
pub const SERVICE_TYPE: &str = "_devdy-sync._tcp.local.";
/// Handshake and control frames.
pub const MAX_CONTROL_FRAME: usize = 64 * 1024;
/// The transfer payload (whole configuration, Skill files and Slack threads included).
pub const MAX_DATA_FRAME: usize = 512 * 1024 * 1024;
/// Digits of the one-time code.
pub const CODE_LEN: usize = 6;

const ID_CLIENT: &[u8] = b"devdy-lan-sync/client";
const ID_SERVER: &[u8] = b"devdy-lan-sync/server";
const CTX_C2S: &[u8] = b"devdy-lan-sync/v1 c2s";
const CTX_S2C: &[u8] = b"devdy-lan-sync/v1 s2c";
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;

const TAG_SEALED: u8 = 1;
const TAG_ERROR: u8 = 2;

/// Reason codes of `Refused` and error frames.
pub mod reason {
    pub const BAD_CODE: &str = "bad_code";
    pub const TOO_MANY_ATTEMPTS: &str = "too_many_attempts";
    pub const UNSUPPORTED_VERSION: &str = "unsupported_version";
    pub const EXPORT_FAILED: &str = "export_failed";
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    pub proto: u32,
    pub device_name: String,
    pub pake: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Reply {
    #[serde(rename_all = "camelCase")]
    Welcome {
        proto: u32,
        device_name: String,
        pake: String,
    },
    Refused {
        reason: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Op {
    pub op: String,
}

pub const OP_PULL: &str = "pull";
pub const OP_ACK: &str = "ack";

/// A post-handshake frame.
#[derive(Debug, PartialEq, Eq)]
pub enum Incoming {
    Data(Vec<u8>),
    Error(String),
}

/// `123456`, `123 456`, `123-456` → `123456`; `None` when it is not a full code.
pub fn normalize_code(raw: &str) -> Option<String> {
    let digits: String = raw
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();
    (digits.len() == CODE_LEN && digits.chars().all(|c| c.is_ascii_digit())).then_some(digits)
}

pub fn generate_code() -> String {
    let mut bytes = [0u8; 8];
    copy_randombytes(&mut bytes);
    let n = u64::from_le_bytes(bytes) % 10u64.pow(CODE_LEN as u32);
    format!("{n:0width$}", width = CODE_LEN)
}

/// One side of the SPAKE2 exchange, waiting for the peer's message.
pub struct Pake {
    inner: Spake2<Ed25519Group>,
    is_client: bool,
}

impl Pake {
    pub fn client(code: &str) -> (Self, Vec<u8>) {
        let (inner, msg) = Spake2::<Ed25519Group>::start_a(
            &Password::new(code.as_bytes()),
            &Identity::new(ID_CLIENT),
            &Identity::new(ID_SERVER),
        );
        (
            Self {
                inner,
                is_client: true,
            },
            msg,
        )
    }

    pub fn server(code: &str) -> (Self, Vec<u8>) {
        let (inner, msg) = Spake2::<Ed25519Group>::start_b(
            &Password::new(code.as_bytes()),
            &Identity::new(ID_CLIENT),
            &Identity::new(ID_SERVER),
        );
        (
            Self {
                inner,
                is_client: false,
            },
            msg,
        )
    }

    /// The channel keys. A wrong code still "succeeds" here: it only shows as the first sealed
    /// frame failing to open on the other side.
    pub fn finish(self, peer_msg: &[u8]) -> Result<Channel, String> {
        let shared = self
            .inner
            .finish(peer_msg)
            .map_err(|_| "Invalid handshake from the other device".to_string())?;
        let c2s = derive_key(&shared, CTX_C2S)?;
        let s2c = derive_key(&shared, CTX_S2C)?;
        Ok(if self.is_client {
            Channel {
                send: c2s,
                recv: s2c,
                send_ctx: CTX_C2S,
                recv_ctx: CTX_S2C,
            }
        } else {
            Channel {
                send: s2c,
                recv: c2s,
                send_ctx: CTX_S2C,
                recv_ctx: CTX_C2S,
            }
        })
    }
}

fn derive_key(shared: &[u8], context: &[u8]) -> Result<Key, String> {
    let mut key: Key = [0u8; 32];
    crypto_generichash(&mut key, context, Some(shared))
        .map_err(|_| "Could not derive the channel key".to_string())?;
    Ok(key)
}

/// The encrypted channel after the handshake. No `Debug`: it holds keys.
pub struct Channel {
    send: Key,
    recv: Key,
    send_ctx: &'static [u8],
    recv_ctx: &'static [u8],
}

impl Channel {
    pub fn seal(&self, plain: &[u8]) -> Vec<u8> {
        let mut nonce: Nonce = [0u8; NONCE_LEN];
        copy_randombytes(&mut nonce);
        let mut out = vec![0u8; 1 + NONCE_LEN + plain.len() + TAG_LEN];
        out[0] = TAG_SEALED;
        out[1..1 + NONCE_LEN].copy_from_slice(&nonce);
        crypto_aead_xchacha20poly1305_ietf_encrypt(
            &mut out[1 + NONCE_LEN..],
            plain,
            Some(self.send_ctx),
            &nonce,
            &self.send,
        )
        .expect("output buffer is sized for the ciphertext");
        out
    }

    /// `Err` when a sealed frame does not open: the two sides used different codes (or the frame
    /// was tampered with).
    pub fn open(&self, frame: &[u8]) -> Result<Incoming, ()> {
        match frame.split_first() {
            Some((&TAG_ERROR, reason)) => Ok(Incoming::Error(
                String::from_utf8_lossy(reason).into_owned(),
            )),
            Some((&TAG_SEALED, rest)) if rest.len() >= NONCE_LEN + TAG_LEN => {
                let (nonce, cipher) = rest.split_at(NONCE_LEN);
                let nonce: Nonce = nonce.try_into().map_err(|_| ())?;
                let mut plain = vec![0u8; cipher.len() - TAG_LEN];
                crypto_aead_xchacha20poly1305_ietf_decrypt(
                    &mut plain,
                    cipher,
                    Some(self.recv_ctx),
                    &nonce,
                    &self.recv,
                )
                .map_err(|_| ())?;
                Ok(Incoming::Data(plain))
            }
            _ => Err(()),
        }
    }
}

pub fn error_frame(reason: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + reason.len());
    out.push(TAG_ERROR);
    out.extend_from_slice(reason.as_bytes());
    out
}

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, data: &[u8]) -> std::io::Result<()> {
    let len = u32::try_from(data.len())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "frame too large"))?;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(data).await?;
    w.flush().await
}

pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R, max: usize) -> std::io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    if len > max {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(client_code: &str, server_code: &str) -> (Channel, Channel) {
        let (client, client_msg) = Pake::client(client_code);
        let (server, server_msg) = Pake::server(server_code);
        (
            client.finish(&server_msg).unwrap(),
            server.finish(&client_msg).unwrap(),
        )
    }

    #[test]
    fn same_code_opens_both_directions() {
        let (client, server) = pair("123456", "123456");
        let frame = client.seal(b"pull");
        assert_eq!(server.open(&frame), Ok(Incoming::Data(b"pull".to_vec())));
        let frame = server.seal(b"payload");
        assert_eq!(client.open(&frame), Ok(Incoming::Data(b"payload".to_vec())));
    }

    #[test]
    fn wrong_code_does_not_open() {
        let (client, server) = pair("123456", "654321");
        assert!(server.open(&client.seal(b"pull")).is_err());
    }

    #[test]
    fn frame_is_not_reflected_back_to_its_sender() {
        let (client, _server) = pair("123456", "123456");
        assert!(client.open(&client.seal(b"pull")).is_err());
    }

    #[test]
    fn tampered_frame_does_not_open() {
        let (client, server) = pair("123456", "123456");
        let mut frame = client.seal(b"pull");
        *frame.last_mut().unwrap() ^= 1;
        assert!(server.open(&frame).is_err());
    }

    #[test]
    fn error_frame_round_trips() {
        let (client, _server) = pair("123456", "123456");
        assert_eq!(
            client.open(&error_frame(reason::BAD_CODE)),
            Ok(Incoming::Error(reason::BAD_CODE.to_string()))
        );
    }

    #[test]
    fn normalizes_codes() {
        assert_eq!(normalize_code(" 123 456 "), Some("123456".to_string()));
        assert_eq!(normalize_code("123-456"), Some("123456".to_string()));
        assert_eq!(normalize_code("12345"), None);
        assert_eq!(normalize_code("12345a"), None);
    }

    #[test]
    fn generated_code_is_six_digits() {
        for _ in 0..50 {
            assert!(normalize_code(&generate_code()).is_some());
        }
    }

    #[tokio::test]
    async fn frames_round_trip_and_enforce_the_limit() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        write_frame(&mut a, b"hello").await.unwrap();
        assert_eq!(read_frame(&mut b, 16).await.unwrap(), b"hello");
        write_frame(&mut a, &[0u8; 32]).await.unwrap();
        assert!(read_frame(&mut b, 16).await.is_err());
    }
}
