//! Device-to-device requests for on-demand sync (design:
//! .workspace/docs/03-design/on-demand-sync.md, §P "relay is a pipe").
//!
//! A group has two devices; either one asks, the other answers. The asking
//! device sends a request (changes since a cursor, a file range, a project
//! folder manifest…) and gets one or more replies back. Messages travel as
//! sealed `sync_peer_send` / `sync_peer_msg` frames through the relay, which
//! only forwards them: it stores nothing. Bulk data travels inside replies,
//! cut into chunks of at most [`CHUNK_SIZE`] bytes ([`encode_chunk`] /
//! [`decode_chunk`]); the asker requests the next chunks as it writes the
//! previous ones. The engine implements [`PeerLink`] over the live relay
//! connection and dispatches incoming requests to the registered
//! [`PeerHandler`]s; [`testing::pair`] links two in-memory ends for tests.

// `#[async_trait]` marks the boxed futures `#[must_use]` a second time.
#![allow(clippy::double_must_use)]

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Plaintext of one peer message (sealed with the group key on the wire).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeerMsg {
    pub v: u32,
    /// Unique per message.
    pub id: String,
    /// Request kind (e.g. `changes`, `folder_manifest`), or a reply kind.
    pub kind: String,
    /// Set on replies: the `id` of the request answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// Replies: this is the last reply for `reply_to`.
    #[serde(default)]
    pub last: bool,
    #[serde(default)]
    pub body: Value,
}

/// Reply kind carrying a failure; its body is `{"message": "..."}`.
pub const ERROR_KIND: &str = "error";

/// A stream of replies to one request: `Ok((kind, body))` per reply, ended
/// after the reply flagged `last`; an [`ERROR_KIND`] reply, a timeout or a
/// lost connection arrive as one `Err` and end the stream.
pub type Replies = mpsc::Receiver<Result<(String, Value), String>>;

/// What a sync feature can do with the other devices of the group.
#[async_trait]
pub trait PeerLink: Send + Sync {
    /// Device ids of the group currently connected to the relay (not self).
    async fn online_peers(&self) -> Result<Vec<String>, String>;
    /// Send a request to `to`. Fails right away with `peer_offline` when the
    /// device is not connected.
    async fn request(&self, to: &str, kind: &str, body: Value) -> Result<Replies, String>;
}

/// Largest piece of file content in one reply. One reply (base64 chunk inside
/// the JSON message, sealed, base64 again) stays far below the relay's frame
/// cap (`RELAY_SYNC_MAX_BLOB_BYTES`, 4 MiB + 64, as base64).
pub const CHUNK_SIZE: usize = 1024 * 1024;

/// How many chunk requests an asker keeps in flight per transfer.
pub const CHUNK_WINDOW: usize = 4;

/// Chunk bytes as a reply body field (`"data"`).
pub fn encode_chunk(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The bytes of a `"data"` field written by [`encode_chunk`].
pub fn decode_chunk(data: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(data).map_err(|e| format!("bad chunk: {e}"))
}

/// Sends the replies of one incoming request.
#[async_trait]
pub trait Reply: Send + Sync {
    async fn send(&self, kind: &str, body: Value, last: bool) -> Result<(), String>;
}

impl dyn Reply {
    pub async fn error(&self, message: impl Into<String>) -> Result<(), String> {
        self.send(ERROR_KIND, serde_json::json!({ "message": message.into() }), true).await
    }
}

/// Serves incoming requests of some kinds on the source device.
#[async_trait]
pub trait PeerHandler: Send + Sync {
    fn kinds(&self) -> &'static [&'static str];
    /// Must eventually send a `last` reply (or an error) through `reply`.
    async fn handle(&self, link: Arc<dyn PeerLink>, from: String, kind: String, body: Value, reply: Arc<dyn Reply>);
}

/// Two in-memory ends wired to each other, for tests of peer features.
#[cfg(test)]
pub mod testing {
    use super::*;
    use std::sync::Mutex;

    pub struct MemLink {
        pub me: String,
        pub other: String,
        handlers: Arc<Mutex<Vec<Arc<dyn PeerHandler>>>>,
        other_link: Mutex<Option<Arc<MemLink>>>,
        pub online: Arc<std::sync::atomic::AtomicBool>,
    }

    impl MemLink {
        pub fn add_handler(&self, h: Arc<dyn PeerHandler>) {
            self.handlers.lock().unwrap().push(h);
        }
    }

    struct ChanReply(mpsc::Sender<Result<(String, Value), String>>);

    #[async_trait]
    impl Reply for ChanReply {
        async fn send(&self, kind: &str, body: Value, _last: bool) -> Result<(), String> {
            let item = if kind == ERROR_KIND {
                Err(body.get("message").and_then(Value::as_str).unwrap_or("error").to_string())
            } else {
                Ok((kind.to_string(), body))
            };
            // Dropping the sender after a `last` reply ends the stream.
            self.0.send(item).await.map_err(|_| "requester gone".to_string())
        }
    }

    #[async_trait]
    impl PeerLink for MemLink {
        async fn online_peers(&self) -> Result<Vec<String>, String> {
            let up = self.online.load(std::sync::atomic::Ordering::SeqCst);
            Ok(if up { vec![self.other.clone()] } else { vec![] })
        }
        async fn request(&self, to: &str, kind: &str, body: Value) -> Result<Replies, String> {
            if to != self.other || !self.online.load(std::sync::atomic::Ordering::SeqCst) {
                return Err("peer_offline".into());
            }
            let other = self.other_link.lock().unwrap().clone().ok_or("peer_offline")?;
            let handler = other
                .handlers
                .lock()
                .unwrap()
                .iter()
                .find(|h| h.kinds().contains(&kind))
                .cloned();
            let (tx, rx) = mpsc::channel(64);
            let Some(handler) = handler else {
                let _ = tx.send(Err(format!("unknown request kind {kind}"))).await;
                return Ok(rx);
            };
            let from = self.me.clone();
            let kind = kind.to_string();
            tokio::spawn(async move {
                let link: Arc<dyn PeerLink> = other.clone();
                handler.handle(link, from, kind, body, Arc::new(ChanReply(tx))).await;
            });
            Ok(rx)
        }
    }

    /// `(a, b)`: a request sent from `a` is served by `b`'s handlers and vice
    /// versa.
    pub fn pair(a: &str, b: &str) -> (Arc<MemLink>, Arc<MemLink>) {
        let online = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let mk = |me: &str, other: &str| {
            Arc::new(MemLink {
                me: me.into(),
                other: other.into(),
                handlers: Default::default(),
                other_link: Mutex::new(None),
                online: online.clone(),
            })
        };
        let (la, lb) = (mk(a, b), mk(b, a));
        *la.other_link.lock().unwrap() = Some(lb.clone());
        *lb.other_link.lock().unwrap() = Some(la.clone());
        (la, lb)
    }
}
