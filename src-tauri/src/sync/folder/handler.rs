//! Source side: serves `folder_manifest`, `folder_fetch` and `folder_done`.
//!
//! A manifest request starts a *transfer*: the manifest JSON, the bundles
//! built for it and the files it lists become contents the receiver pulls
//! range by range. Bundle files live as long as the transfer: until
//! `folder_done`, a newer manifest of the same receiver + project, or
//! [`IDLE_TTL`] without a request.

use super::manifest::{self, Content, ManifestRequest, TempFiles};
use super::{blocking, IDLE_TTL, KIND_DONE, KIND_FETCH, KIND_MANIFEST, MAX_FETCH_ITEMS, REPLY_CHUNK, REPLY_DONE, REPLY_MANIFEST};
use crate::sync::peer::{encode_chunk, PeerHandler, PeerLink, Reply, CHUNK_SIZE};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

/// Where a project lives on the source device.
// async_trait's generated futures trip clippy::double_must_use (as elsewhere).
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait RootResolver: Send + Sync {
    async fn root(&self, project_id: &str) -> Result<PathBuf, String>;
}

/// `folder_manifest_ready` reply body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestReady {
    pub transfer: String,
    /// The manifest JSON as a content (fetch the rest after `data`).
    pub manifest: ContentRef,
    /// The first bytes of the manifest JSON (≤ `CHUNK_SIZE`), base64.
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentRef {
    pub sha256: String,
    pub size: u64,
}

/// `folder_fetch` request body.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FetchRequest {
    pub transfer: String,
    /// Content ranges (at most [`MAX_FETCH_ITEMS`], `CHUNK_SIZE` bytes in
    /// total), answered in order by one `folder_chunk` reply.
    pub items: Vec<FetchItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchItem {
    pub sha: String,
    pub offset: u64,
    pub len: u64,
}

/// One range of a `folder_chunk` reply (`{"chunks": [...]}`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub sha: String,
    pub offset: u64,
    /// base64 ([`encode_chunk`]).
    pub data: String,
}

/// `folder_done` request body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoneRequest {
    pub transfer: String,
}

/// What one transfer serves (bundle files deleted when the last user goes).
struct Data {
    manifest_sha: String,
    manifest: Vec<u8>,
    contents: HashMap<String, Content>,
    _temps: TempFiles,
}

struct Transfer {
    from: String,
    project_id: String,
    last_used: Instant,
    data: Arc<Data>,
}

type Transfers = Arc<Mutex<HashMap<String, Transfer>>>;

pub struct FolderSource {
    roots: Arc<dyn RootResolver>,
    tmp_dir: PathBuf,
    idle_ttl: Duration,
    transfers: Transfers,
}

impl FolderSource {
    pub fn new(roots: Arc<dyn RootResolver>) -> Self {
        let tmp_dir = std::env::temp_dir().join(format!("devdy-folder-sync-{}", std::process::id()));
        Self::with(roots, tmp_dir, IDLE_TTL)
    }

    /// Bundles under `tmp_dir`, transfers dropped after `idle_ttl`.
    pub fn with(roots: Arc<dyn RootResolver>, tmp_dir: PathBuf, idle_ttl: Duration) -> Self {
        Self { roots, tmp_dir, idle_ttl, transfers: Default::default() }
    }

    /// Transfers currently kept.
    #[cfg(test)]
    pub fn transfers(&self) -> usize {
        self.transfers.lock().unwrap().len()
    }

    async fn manifest(&self, from: &str, body: Value, reply: &Arc<dyn Reply>) -> Result<(), String> {
        let req: ManifestRequest = serde_json::from_value(body).map_err(|e| format!("bad folder_manifest request: {e}"))?;
        let root = self.roots.root(&req.project_id).await?;
        // A new manifest supersedes the previous one (and its bundles).
        let old: Vec<Transfer> = {
            let mut t = self.transfers.lock().unwrap();
            let ids: Vec<String> = t.iter().filter(|(_, x)| x.from == from && x.project_id == req.project_id).map(|(id, _)| id.clone()).collect();
            ids.iter().filter_map(|id| t.remove(id)).collect()
        };
        drop(old);
        let tmp = self.tmp_dir.clone();
        let req2 = req.clone();
        let built = blocking(move || {
            let root = root.canonicalize().map_err(|_| "project folder is missing on the source device".to_string())?;
            manifest::build(&root, &req2, &tmp)
        })
        .await?;
        let json = serde_json::to_vec(&built.manifest).map_err(|e| e.to_string())?;
        let manifest_sha = hex::encode(Sha256::digest(&json));
        let ready = ManifestReady {
            transfer: uuid::Uuid::new_v4().simple().to_string(),
            manifest: ContentRef { sha256: manifest_sha.clone(), size: json.len() as u64 },
            data: encode_chunk(&json[..json.len().min(CHUNK_SIZE)]),
        };
        let data = Arc::new(Data { manifest_sha, manifest: json, contents: built.contents, _temps: built.temps });
        let id = ready.transfer.clone();
        self.transfers.lock().unwrap().insert(
            id.clone(),
            Transfer { from: from.into(), project_id: req.project_id.clone(), last_used: Instant::now(), data },
        );
        self.reap_when_idle(&id);
        if let Err(e) = reply.send(REPLY_MANIFEST, serde_json::to_value(&ready).unwrap(), true).await {
            let gone = self.transfers.lock().unwrap().remove(&id);
            drop(gone);
            return Err(e);
        }
        Ok(())
    }

    /// Drops the transfer once nobody used it for `idle_ttl` (the receiver
    /// crashed, lost the connection, ...).
    fn reap_when_idle(&self, id: &str) {
        let (weak, ttl, id): (Weak<_>, _, _) = (Arc::downgrade(&self.transfers), self.idle_ttl, id.to_string());
        let Ok(rt) = tokio::runtime::Handle::try_current() else { return };
        rt.spawn(async move {
            loop {
                tokio::time::sleep((ttl / 4).max(Duration::from_millis(10))).await;
                let Some(map) = weak.upgrade() else { return };
                let expired = {
                    let mut m = map.lock().unwrap();
                    match m.get(&id) {
                        None => return,
                        Some(t) if t.last_used.elapsed() >= ttl => m.remove(&id),
                        Some(_) => None,
                    }
                };
                if expired.is_some() {
                    tracing::info!(event = "folder_sync_transfer_expired");
                    return;
                }
            }
        });
    }

    fn data(&self, from: &str, transfer: &str) -> Result<Arc<Data>, String> {
        let mut m = self.transfers.lock().unwrap();
        match m.get_mut(transfer) {
            Some(t) if t.from == from => {
                t.last_used = Instant::now();
                Ok(t.data.clone())
            }
            _ => Err("no folder manifest for this project; start the sync again".into()),
        }
    }

    async fn fetch(&self, from: &str, body: Value, reply: &Arc<dyn Reply>) -> Result<(), String> {
        let req: FetchRequest = serde_json::from_value(body).map_err(|e| format!("bad folder_fetch request: {e}"))?;
        if req.items.len() > MAX_FETCH_ITEMS || req.items.iter().try_fold(0u64, |n, i| n.checked_add(i.len)).is_none_or(|n| n > CHUNK_SIZE as u64) {
            return Err("folder_fetch asks for too much at once".into());
        }
        let data = self.data(from, &req.transfer)?;
        let mut chunks = Vec::with_capacity(req.items.len());
        for it in req.items {
            let bytes = read_range(&data, &it).await?;
            chunks.push(Chunk { sha: it.sha, offset: it.offset, data: encode_chunk(&bytes) });
        }
        reply.send(REPLY_CHUNK, json!({ "chunks": chunks }), true).await
    }

    fn done(&self, from: &str, body: Value) -> Result<(), String> {
        let req: DoneRequest = serde_json::from_value(body).map_err(|e| format!("bad folder_done request: {e}"))?;
        let gone = {
            let mut m = self.transfers.lock().unwrap();
            match m.get(&req.transfer) {
                Some(t) if t.from == from => m.remove(&req.transfer),
                _ => None,
            }
        };
        drop(gone);
        Ok(())
    }
}

/// Reads only (the source's files are never written).
async fn read_range(data: &Data, it: &FetchItem) -> Result<Vec<u8>, String> {
    let (size, file) = if it.sha == data.manifest_sha {
        (data.manifest.len() as u64, None)
    } else {
        let c = data.contents.get(&it.sha).ok_or_else(|| format!("unknown content {}", short(&it.sha)))?;
        (c.size, Some(&c.path))
    };
    let end = it.offset.checked_add(it.len).filter(|&e| e <= size).ok_or_else(|| format!("bad range for {}", short(&it.sha)))?;
    let Some(path) = file else {
        return Ok(data.manifest[it.offset as usize..end as usize].to_vec());
    };
    let changed = || "a file changed on the source during the sync; sync again".to_string();
    let mut f = tokio::fs::File::open(path).await.map_err(|_| changed())?;
    f.seek(std::io::SeekFrom::Start(it.offset)).await.map_err(|_| changed())?;
    let mut buf = vec![0u8; it.len as usize];
    f.read_exact(&mut buf).await.map_err(|_| changed())?;
    Ok(buf)
}

pub(crate) fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

#[async_trait]
impl PeerHandler for FolderSource {
    fn kinds(&self) -> &'static [&'static str] {
        &[KIND_MANIFEST, KIND_FETCH, KIND_DONE]
    }

    async fn handle(&self, _link: Arc<dyn PeerLink>, from: String, kind: String, body: Value, reply: Arc<dyn Reply>) {
        let res = match kind.as_str() {
            KIND_MANIFEST => self.manifest(&from, body, &reply).await,
            KIND_FETCH => self.fetch(&from, body, &reply).await,
            KIND_DONE => match self.done(&from, body) {
                Ok(()) => reply.send(REPLY_DONE, json!({}), true).await,
                Err(e) => Err(e),
            },
            other => Err(format!("unknown request kind {other}")),
        };
        if let Err(e) = res {
            tracing::warn!(event = "folder_sync_serve_failed", kind = %kind, error = %e);
            let _ = reply.error(e).await;
        }
    }
}
