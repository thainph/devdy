//! Receiver side of the wire: get the manifest, then pull contents in
//! batches (≤ [`BATCH_BYTES`]) into a staging dir, verifying sha256.

use super::handler::{short, Chunk, FetchItem, FetchRequest};
use super::manifest::{Manifest, ManifestRequest};
use super::{Cancel, BATCH_BYTES, KIND_FETCH, KIND_MANIFEST, REPLY_CHUNK, REPLY_FETCH_DONE, REPLY_MANIFEST};
use crate::sync::peer::PeerLink;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub async fn request_manifest(link: &Arc<dyn PeerLink>, source: &str, req: &ManifestRequest) -> Result<Manifest, String> {
    let mut rx = link.request(source, KIND_MANIFEST, serde_json::to_value(req).unwrap()).await?;
    let body = loop {
        match rx.recv().await {
            Some(Ok((kind, body))) if kind == REPLY_MANIFEST => break body,
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(e),
            None => return Err("the source device stopped answering".into()),
        }
    };
    let blobs: Vec<String> = serde_json::from_value(body.get("manifest_blobs").cloned().unwrap_or(Value::Null))
        .map_err(|e| format!("bad manifest reply: {e}"))?;
    let mut bytes = Vec::new();
    let mut res = Ok(());
    for b in &blobs {
        match link.blob_get(b).await {
            Ok(p) => bytes.extend_from_slice(&p),
            Err(e) => {
                res = Err(e);
                break;
            }
        }
    }
    let _ = link.blob_release(&blobs).await;
    res?;
    let m: Manifest = serde_json::from_slice(&bytes).map_err(|e| format!("bad folder manifest: {e}"))?;
    m.validate()?;
    Ok(m)
}

/// Contents being downloaded into `<staging>/<sha>.part`.
struct Partial {
    file: std::fs::File,
    hasher: Sha256,
    written: u64,
}

/// Downloads contents into a staging directory; finished contents end up
/// as `<staging>/<sha>` (verified).
pub struct Downloader {
    pub staging: PathBuf,
    partial: HashMap<String, Partial>,
}

impl Downloader {
    pub fn new(staging: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(staging).map_err(|e| format!("staging dir: {e}"))?;
        Ok(Self { staging: staging.to_path_buf(), partial: HashMap::new() })
    }

    pub fn done_path(&self, sha: &str) -> PathBuf {
        self.staging.join(sha)
    }

    /// Fetch `wanted` (sha → size) from the source. `on_progress(bytes)` after
    /// each chunk; `on_done(sha)` when a content is complete and verified.
    #[allow(clippy::too_many_arguments)]
    pub async fn fetch(
        &mut self,
        link: &Arc<dyn PeerLink>,
        source: &str,
        project_id: &str,
        wanted: &[(String, u64)],
        cancel: &Cancel,
        on_progress: &mut (dyn FnMut(u64) + Send),
        on_done: &mut (dyn FnMut(&str, &Path) -> Result<(), String> + Send),
    ) -> Result<(), String> {
        for batch in batches(wanted) {
            cancel.check()?;
            let req = FetchRequest { project_id: project_id.into(), items: batch.clone(), shas: vec![] };
            let mut rx = link.request(source, KIND_FETCH, serde_json::to_value(&req).unwrap()).await?;
            let mut next = 0usize;
            loop {
                let (kind, body) = match rx.recv().await {
                    Some(Ok(r)) => r,
                    Some(Err(e)) => return Err(e),
                    None => return Err("the source device stopped answering".into()),
                };
                if kind == REPLY_FETCH_DONE {
                    break;
                }
                if kind != REPLY_CHUNK {
                    continue;
                }
                let chunk: Chunk = serde_json::from_value(body).map_err(|e| format!("bad chunk reply: {e}"))?;
                let res = match batch.get(next) {
                    Some(it) if it.sha == chunk.sha && it.offset == chunk.offset && it.len == chunk.len => {
                        self.take_chunk(link, &chunk, cancel, on_progress).await
                    }
                    _ => Err("unexpected chunk from the source".into()),
                };
                let _ = link.blob_release(&chunk.blobs).await;
                res?;
                next += 1;
                let size = wanted.iter().find(|(s, _)| *s == chunk.sha).map(|(_, n)| *n).unwrap_or(0);
                if chunk.offset + chunk.len == size {
                    let p = self.finish(&chunk.sha, size)?;
                    on_done(&chunk.sha, &p)?;
                }
            }
            if next != batch.len() {
                return Err("the source sent an incomplete batch".into());
            }
        }
        Ok(())
    }

    async fn take_chunk(&mut self, link: &Arc<dyn PeerLink>, c: &Chunk, cancel: &Cancel, on_progress: &mut (dyn FnMut(u64) + Send)) -> Result<(), String> {
        if c.offset == 0 {
            let path = self.staging.join(format!("{}.part", c.sha));
            let file = std::fs::File::create(&path).map_err(|e| format!("staging: {e}"))?;
            self.partial.insert(c.sha.clone(), Partial { file, hasher: Sha256::new(), written: 0 });
        }
        let p = self.partial.get_mut(&c.sha).ok_or("chunk out of order")?;
        if p.written != c.offset {
            return Err("chunk out of order".into());
        }
        let mut got = 0u64;
        for b in &c.blobs {
            cancel.check()?;
            let data = link.blob_get(b).await?;
            p.file.write_all(&data).map_err(|e| format!("staging write: {e}"))?;
            p.hasher.update(&data);
            p.written += data.len() as u64;
            got += data.len() as u64;
            on_progress(data.len() as u64);
        }
        if got != c.len {
            return Err(format!("short chunk for {}", short(&c.sha)));
        }
        Ok(())
    }

    fn finish(&mut self, sha: &str, size: u64) -> Result<PathBuf, String> {
        let p = self.partial.remove(sha).ok_or("missing content")?;
        drop(p.file);
        let part = self.staging.join(format!("{sha}.part"));
        let got = hex::encode(p.hasher.finalize());
        if got != sha || p.written != size {
            let _ = std::fs::remove_file(&part);
            return Err(format!("content {} failed verification (changed on the source?); sync again", short(sha)));
        }
        let done = self.done_path(sha);
        std::fs::rename(&part, &done).map_err(|e| format!("staging: {e}"))?;
        Ok(done)
    }
}

/// Split contents into ranges and pack them into requests of at most
/// [`BATCH_BYTES`]; a content larger than that spans several batches.
pub fn batches(wanted: &[(String, u64)]) -> Vec<Vec<FetchItem>> {
    batches_of(wanted, BATCH_BYTES)
}

pub fn batches_of(wanted: &[(String, u64)], cap: u64) -> Vec<Vec<FetchItem>> {
    let mut out: Vec<Vec<FetchItem>> = Vec::new();
    let mut cur: Vec<FetchItem> = Vec::new();
    let mut cur_bytes = 0u64;
    for (sha, size) in wanted {
        let mut off = 0u64;
        while off < *size {
            let room = cap - cur_bytes;
            let len = (size - off).min(room);
            cur.push(FetchItem { sha: sha.clone(), offset: off, len });
            cur_bytes += len;
            off += len;
            if cur_bytes == cap {
                out.push(std::mem::take(&mut cur));
                cur_bytes = 0;
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches_respect_cap_and_split_large_contents() {
        let w = vec![("a".to_string(), 3u64), ("b".to_string(), 10), ("c".to_string(), 2)];
        let b = batches_of(&w, 4);
        for batch in &b {
            assert!(batch.iter().map(|i| i.len).sum::<u64>() <= 4);
        }
        let total: u64 = b.iter().flatten().map(|i| i.len).sum();
        assert_eq!(total, 15);
        let bs: Vec<_> = b.iter().flatten().filter(|i| i.sha == "b").map(|i| (i.offset, i.len)).collect();
        assert_eq!(bs, vec![(0, 1), (1, 4), (5, 4), (9, 1)]);
    }
}
