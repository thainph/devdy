//! Source side: serves `folder_manifest` and `folder_fetch`.

use super::manifest::{self, Content, ManifestRequest, TempFiles};
use super::{blocking, KIND_FETCH, KIND_MANIFEST, REPLY_CHUNK, REPLY_FETCH_DONE, REPLY_MANIFEST};
use crate::sync::peer::{PeerHandler, PeerLink, Reply, MAX_BLOB_PLAINTEXT};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

/// Where a project lives on the source device.
// async_trait's generated futures trip clippy::double_must_use (as elsewhere).
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait RootResolver: Send + Sync {
    async fn root(&self, project_id: &str) -> Result<PathBuf, String>;
}

/// `folder_fetch` request body.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FetchRequest {
    pub project_id: String,
    /// Content ranges, served (and replied) in order.
    #[serde(default)]
    pub items: Vec<FetchItem>,
    /// Whole contents (shorthand for items covering each content).
    #[serde(default)]
    pub shas: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchItem {
    pub sha: String,
    pub offset: u64,
    pub len: u64,
}

/// One `folder_chunk` reply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub sha: String,
    pub offset: u64,
    pub len: u64,
    pub blobs: Vec<String>,
}

/// Contents of the last manifest built for one receiver + project (its
/// bundle files go with it).
struct Served {
    contents: HashMap<String, Content>,
    _temps: TempFiles,
}

/// Blobs uploaded but not handed to the receiver yet: released if the
/// request is aborted (or fails) before they are.
struct Pending {
    link: Arc<dyn PeerLink>,
    shas: Vec<String>,
}

impl Pending {
    fn handed_over(mut self) -> Vec<String> {
        std::mem::take(&mut self.shas)
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if self.shas.is_empty() {
            return;
        }
        let (link, shas) = (self.link.clone(), std::mem::take(&mut self.shas));
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let _ = link.blob_release(&shas).await;
            });
        }
    }
}

pub struct FolderSource {
    roots: Arc<dyn RootResolver>,
    tmp_dir: PathBuf,
    served: Mutex<HashMap<String, Arc<Served>>>,
}

impl FolderSource {
    pub fn new(roots: Arc<dyn RootResolver>) -> Self {
        let tmp_dir = std::env::temp_dir().join(format!("devdy-folder-sync-{}", std::process::id()));
        Self { roots, tmp_dir, served: Mutex::new(HashMap::new()) }
    }

    async fn manifest(&self, link: &Arc<dyn PeerLink>, from: &str, body: Value, reply: &Arc<dyn Reply>) -> Result<(), String> {
        let req: ManifestRequest = serde_json::from_value(body).map_err(|e| format!("bad folder_manifest request: {e}"))?;
        let root = self.roots.root(&req.project_id).await?;
        let key = format!("{from}\0{}", req.project_id);
        // A new manifest supersedes the previous one (and its bundles).
        self.served.lock().unwrap().remove(&key);
        let tmp = self.tmp_dir.clone();
        let req2 = req.clone();
        let built = blocking(move || {
            let root = root.canonicalize().map_err(|_| "project folder is missing on the source device".to_string())?;
            manifest::build(&root, &req2, &tmp)
        })
        .await?;
        let served = Arc::new(Served { contents: built.contents, _temps: built.temps });
        let json = serde_json::to_vec(&built.manifest).map_err(|e| e.to_string())?;
        let mut pending = Pending { link: link.clone(), shas: Vec::new() };
        for part in json.chunks(MAX_BLOB_PLAINTEXT) {
            pending.shas.push(link.blob_put(part.to_vec()).await?);
        }
        let bundles: serde_json::Map<String, Value> = built
            .manifest
            .repos
            .iter()
            .filter_map(|r| r.bundle.as_ref().map(|b| (r.path.clone(), json!({ "sha256": b.sha256, "size": b.size }))))
            .collect();
        self.served.lock().unwrap().insert(key, served);
        reply.send(REPLY_MANIFEST, json!({ "manifest_blobs": pending.shas, "bundles": bundles }), true).await?;
        pending.handed_over();
        Ok(())
    }

    async fn fetch(&self, link: &Arc<dyn PeerLink>, from: &str, body: Value, reply: &Arc<dyn Reply>) -> Result<(), String> {
        let req: FetchRequest = serde_json::from_value(body).map_err(|e| format!("bad folder_fetch request: {e}"))?;
        let served = self
            .served
            .lock()
            .unwrap()
            .get(&format!("{from}\0{}", req.project_id))
            .cloned()
            .ok_or("no folder manifest for this project; start the sync again")?;
        let mut items = req.items;
        for sha in req.shas {
            let size = served.contents.get(&sha).map(|c| c.size).unwrap_or(0);
            items.push(FetchItem { sha, offset: 0, len: size });
        }
        for it in items {
            let c = served.contents.get(&it.sha).ok_or_else(|| format!("unknown content {}", short(&it.sha)))?;
            if it.offset.checked_add(it.len).is_none_or(|end| end > c.size) {
                return Err(format!("bad range for {}", short(&it.sha)));
            }
            let pending = upload_range(link, c, it.offset, it.len).await?;
            let chunk = Chunk { sha: it.sha, offset: it.offset, len: it.len, blobs: pending.shas.clone() };
            reply.send(REPLY_CHUNK, serde_json::to_value(&chunk).unwrap(), false).await?;
            pending.handed_over();
        }
        reply.send(REPLY_FETCH_DONE, json!({}), true).await
    }
}

/// Reads only (the source's files are never written); the blobs are
/// released unless handed over.
async fn upload_range(link: &Arc<dyn PeerLink>, c: &Content, offset: u64, len: u64) -> Result<Pending, String> {
    let changed = || "a file changed on the source during the sync; sync again".to_string();
    let mut f = tokio::fs::File::open(&c.path).await.map_err(|_| changed())?;
    f.seek(std::io::SeekFrom::Start(offset)).await.map_err(|_| changed())?;
    let mut blobs = Pending { link: link.clone(), shas: Vec::new() };
    let mut left = len;
    while left > 0 {
        let n = left.min(MAX_BLOB_PLAINTEXT as u64) as usize;
        let mut buf = vec![0u8; n];
        f.read_exact(&mut buf).await.map_err(|_| changed())?;
        blobs.shas.push(link.blob_put(buf).await?);
        left -= n as u64;
    }
    Ok(blobs)
}

pub(crate) fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

#[async_trait]
impl PeerHandler for FolderSource {
    fn kinds(&self) -> &'static [&'static str] {
        &[KIND_MANIFEST, KIND_FETCH]
    }

    async fn handle(&self, link: Arc<dyn PeerLink>, from: String, kind: String, body: Value, reply: Arc<dyn Reply>) {
        let res = match kind.as_str() {
            KIND_MANIFEST => self.manifest(&link, &from, body, &reply).await,
            KIND_FETCH => self.fetch(&link, &from, body, &reply).await,
            other => Err(format!("unknown request kind {other}")),
        };
        if let Err(e) = res {
            tracing::warn!(event = "folder_sync_serve_failed", kind = %kind, error = %e);
            let _ = reply.error(e).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::peer::testing::pair;

    #[tokio::test]
    async fn blobs_not_handed_over_are_released() {
        let (a, _b) = pair("a", "b");
        let link: Arc<dyn PeerLink> = a.clone();
        let mut p = Pending { link: link.clone(), shas: vec![] };
        p.shas.push(link.blob_put(b"x".to_vec()).await.unwrap());
        let kept = Pending { link: link.clone(), shas: vec![link.blob_put(b"y".to_vec()).await.unwrap()] };
        drop(p); // e.g. the handler task aborted mid-batch
        assert_eq!(kept.handed_over().len(), 1);
        for _ in 0..50 {
            if a.blob_count() == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(a.blob_count(), 1, "only the handed-over blob is left");
    }
}
