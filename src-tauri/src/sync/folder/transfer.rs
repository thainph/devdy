//! Receiver side of the wire: get the manifest, then pull contents range by
//! range (≤ `CHUNK_SIZE` per request, `CHUNK_WINDOW` requests in flight)
//! into a staging dir, verifying sha256. Partly downloaded contents stay in
//! a partial dir so a later sync resumes them.

use super::handler::{short, Chunk, ContentRef, DoneRequest, FetchItem, FetchRequest, ManifestReady};
use super::manifest::{is_sha256, Manifest, ManifestRequest};
use super::{blocking, Cancel, KIND_DONE, KIND_FETCH, KIND_MANIFEST, MAX_FETCH_ITEMS, REPLY_CHUNK, REPLY_MANIFEST};
use crate::sync::peer::{decode_chunk, PeerLink, Replies, CHUNK_SIZE, CHUNK_WINDOW};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Largest manifest JSON accepted (it is kept in memory).
pub const MAX_MANIFEST_BYTES: u64 = 256 * 1024 * 1024;

/// One transfer on the source; ended (`folder_done`) when dropped, so the
/// source frees its bundles on success, error and cancel alike.
pub struct Transfer {
    link: Arc<dyn PeerLink>,
    source: String,
    pub id: String,
}

impl Drop for Transfer {
    fn drop(&mut self) {
        let (link, source) = (self.link.clone(), self.source.clone());
        let body = serde_json::to_value(DoneRequest { transfer: std::mem::take(&mut self.id) }).unwrap();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                if let Ok(mut rx) = link.request(&source, KIND_DONE, body).await {
                    let _ = rx.recv().await;
                }
            });
        }
    }
}

async fn first_reply(rx: &mut Replies) -> Result<(String, serde_json::Value), String> {
    match rx.recv().await {
        Some(r) => r,
        None => Err("the source device stopped answering".into()),
    }
}

pub async fn request_manifest(link: &Arc<dyn PeerLink>, source: &str, req: &ManifestRequest, cancel: &Cancel) -> Result<(Manifest, Transfer), String> {
    let mut rx = link.request(source, KIND_MANIFEST, serde_json::to_value(req).unwrap()).await?;
    let body = loop {
        match first_reply(&mut rx).await? {
            (kind, body) if kind == REPLY_MANIFEST => break body,
            _ => continue,
        }
    };
    let ready: ManifestReady = serde_json::from_value(body).map_err(|e| format!("bad manifest reply: {e}"))?;
    let transfer = Transfer { link: link.clone(), source: source.into(), id: ready.transfer };
    let ContentRef { sha256, size } = ready.manifest;
    if !is_sha256(&sha256) || size > MAX_MANIFEST_BYTES {
        return Err("bad manifest reply".into());
    }
    let mut bytes = decode_chunk(&ready.data)?;
    if bytes.len() as u64 > size {
        return Err("bad manifest reply".into());
    }
    let rest = vec![FetchItem { sha: sha256.clone(), offset: bytes.len() as u64, len: size - bytes.len() as u64 }];
    fetch_ranges(link, source, &transfer.id, rest, cancel, &mut |_, data| {
        bytes.extend_from_slice(&data);
        Ok(())
    })
    .await?;
    if hex::encode(Sha256::digest(&bytes)) != sha256 {
        return Err("the folder manifest failed verification; sync again".into());
    }
    let m: Manifest = serde_json::from_slice(&bytes).map_err(|e| format!("bad folder manifest: {e}"))?;
    m.validate()?;
    Ok((m, transfer))
}

/// Cut `ranges` into pieces of at most `CHUNK_SIZE` and pack them into
/// requests of at most `CHUNK_SIZE` bytes / [`MAX_FETCH_ITEMS`] items.
pub fn pack(ranges: &[FetchItem]) -> Vec<Vec<FetchItem>> {
    pack_of(ranges, CHUNK_SIZE as u64, MAX_FETCH_ITEMS)
}

pub fn pack_of(ranges: &[FetchItem], cap: u64, max_items: usize) -> Vec<Vec<FetchItem>> {
    let mut out: Vec<Vec<FetchItem>> = Vec::new();
    let mut cur: Vec<FetchItem> = Vec::new();
    let mut cur_bytes = 0u64;
    for r in ranges {
        let (mut off, end) = (r.offset, r.offset + r.len);
        while off < end {
            let len = (end - off).min(cap - cur_bytes);
            cur.push(FetchItem { sha: r.sha.clone(), offset: off, len });
            cur_bytes += len;
            off += len;
            if cur_bytes == cap || cur.len() == max_items {
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

#[derive(Deserialize)]
struct Chunks {
    chunks: Vec<Chunk>,
}

/// Takes each piece of a [`fetch_ranges`] in order.
pub type OnData<'a> = dyn FnMut(&FetchItem, Vec<u8>) -> Result<(), String> + Send + 'a;

/// Pull `ranges` of `transfer`, keeping [`CHUNK_WINDOW`] requests in
/// flight; `on_data` gets every piece in order.
pub async fn fetch_ranges(
    link: &Arc<dyn PeerLink>,
    source: &str,
    transfer: &str,
    ranges: Vec<FetchItem>,
    cancel: &Cancel,
    on_data: &mut OnData<'_>,
) -> Result<(), String> {
    let mut todo = pack(&ranges).into_iter();
    let mut flight: VecDeque<(Vec<FetchItem>, Replies)> = VecDeque::new();
    loop {
        while flight.len() < CHUNK_WINDOW {
            let Some(items) = todo.next() else { break };
            cancel.check()?;
            let req = FetchRequest { transfer: transfer.into(), items };
            let rx = link.request(source, KIND_FETCH, serde_json::to_value(&req).unwrap()).await?;
            flight.push_back((req.items, rx));
        }
        let Some((items, mut rx)) = flight.pop_front() else { return Ok(()) };
        let (kind, body) = first_reply(&mut rx).await?;
        if kind != REPLY_CHUNK {
            return Err(format!("unexpected reply {kind} from the source"));
        }
        let got: Chunks = serde_json::from_value(body).map_err(|e| format!("bad chunk reply: {e}"))?;
        if got.chunks.len() != items.len() {
            return Err("the source sent an incomplete chunk reply".into());
        }
        for (it, c) in items.iter().zip(got.chunks) {
            cancel.check()?;
            let data = decode_chunk(&c.data)?;
            if c.sha != it.sha || c.offset != it.offset || data.len() as u64 != it.len {
                return Err(format!("unexpected chunk for {} from the source", short(&it.sha)));
            }
            on_data(it, data)?;
        }
    }
}

/// A content being written to `<partial>/<sha>.part`.
struct Partial {
    file: std::fs::File,
    hasher: Sha256,
    written: u64,
    size: u64,
}

/// Downloads contents into a staging directory; finished contents end up
/// as `<staging>/<sha>` (verified). Unfinished ones stay in `partial` and
/// are resumed by the next download of the same content.
pub struct Downloader {
    pub staging: PathBuf,
    pub partial: PathBuf,
    /// Bytes received from the source (resumed prefixes not counted).
    pub downloaded: u64,
}

/// `<dir>/<sha><suffix>`: only for well-formed content ids, and always a
/// direct child of `dir`.
fn child(dir: &Path, sha: &str, suffix: &str) -> Result<PathBuf, String> {
    if !is_sha256(sha) {
        return Err("bad content id".into());
    }
    let p = dir.join(format!("{sha}{suffix}"));
    if p.parent() != Some(dir) {
        return Err("bad content id".into());
    }
    Ok(p)
}

impl Downloader {
    pub fn new(staging: &Path, partial: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(staging).map_err(|e| format!("staging dir: {e}"))?;
        std::fs::create_dir_all(partial).map_err(|e| format!("staging dir: {e}"))?;
        Ok(Self { staging: staging.to_path_buf(), partial: partial.to_path_buf(), downloaded: 0 })
    }

    pub fn done_path(&self, sha: &str) -> Result<PathBuf, String> {
        child(&self.staging, sha, "")
    }

    /// Fetch `wanted` (sha → size) from the source. `on_progress(bytes)` as
    /// bytes arrive (or are resumed); `on_done(sha)` when a content is
    /// complete and verified.
    #[allow(clippy::too_many_arguments)]
    pub async fn fetch(
        &mut self,
        link: &Arc<dyn PeerLink>,
        source: &str,
        transfer: &Transfer,
        wanted: &[(String, u64)],
        cancel: &Cancel,
        on_progress: &mut (dyn FnMut(u64) + Send),
        on_done: &mut (dyn FnMut(&str, &Path) -> Result<(), String> + Send),
    ) -> Result<(), String> {
        for (sha, _) in wanted {
            child(&self.partial, sha, ".part")?;
        }
        let mut open: HashMap<String, Partial> = HashMap::new();
        let mut ranges = Vec::new();
        for (sha, size) in wanted {
            cancel.check()?;
            if open.contains_key(sha) {
                continue;
            }
            let path = child(&self.partial, sha, ".part")?;
            let size = *size;
            let p = blocking(move || resume(&path, size)).await?;
            on_progress(p.written);
            if p.written == size {
                let done = finish(&self.partial, &self.staging, sha, p)?;
                on_done(sha, &done)?;
                continue;
            }
            ranges.push(FetchItem { sha: sha.clone(), offset: p.written, len: size - p.written });
            open.insert(sha.clone(), p);
        }
        let (partial, staging, downloaded) = (&self.partial, &self.staging, &mut self.downloaded);
        fetch_ranges(link, source, &transfer.id, ranges, cancel, &mut |it, data| {
            let p = open.get_mut(&it.sha).ok_or("chunk for an unknown content")?;
            if p.written != it.offset {
                return Err("chunk out of order".into());
            }
            p.file.write_all(&data).map_err(|e| format!("staging write: {e}"))?;
            p.hasher.update(&data);
            p.written += data.len() as u64;
            *downloaded += data.len() as u64;
            on_progress(data.len() as u64);
            if p.written == p.size {
                let p = open.remove(&it.sha).unwrap();
                let done = finish(partial, staging, &it.sha, p)?;
                on_done(&it.sha, &done)?;
            }
            Ok(())
        })
        .await
    }
}

/// Open `path` for appending, hashing the prefix already there (an empty
/// start when it is longer than the content).
fn resume(path: &Path, size: u64) -> Result<Partial, String> {
    let io = |e: std::io::Error| format!("staging: {e}");
    let have = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut hasher = Sha256::new();
    let written = if have > 0 && have <= size {
        let mut f = std::fs::File::open(path).map_err(io)?;
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = f.read(&mut buf).map_err(io)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        have
    } else {
        std::fs::File::create(path).map_err(io)?;
        0
    };
    let file = std::fs::OpenOptions::new().append(true).open(path).map_err(io)?;
    Ok(Partial { file, hasher, written, size })
}

fn finish(partial: &Path, staging: &Path, sha: &str, p: Partial) -> Result<PathBuf, String> {
    drop(p.file);
    let part = child(partial, sha, ".part")?;
    let got = hex::encode(p.hasher.finalize());
    if got != sha || p.written != p.size {
        let _ = std::fs::remove_file(&part);
        return Err(format!("content {} failed verification (changed on the source?); sync again", short(sha)));
    }
    let done = child(staging, sha, "")?;
    std::fs::rename(&part, &done).map_err(|e| format!("staging: {e}"))?;
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_paths_stay_in_the_staging_dir() {
        let d = Downloader { staging: PathBuf::from("/tmp/stage"), partial: PathBuf::from("/tmp/part"), downloaded: 0 };
        for bad in ["../../etc/passwd", "/etc/passwd", "", "ab", &"A".repeat(64), &format!("{}/x", "a".repeat(62))] {
            assert!(d.done_path(bad).is_err(), "{bad}");
            assert!(child(&d.partial, bad, ".part").is_err(), "{bad}");
        }
        assert_eq!(d.done_path(&"a".repeat(64)).unwrap(), PathBuf::from("/tmp/stage").join("a".repeat(64)));
    }

    #[test]
    fn requests_respect_cap_and_split_large_contents() {
        let r = |sha: &str, offset, len| FetchItem { sha: sha.into(), offset, len };
        let b = pack_of(&[r("a", 0, 3), r("b", 2, 10), r("c", 0, 2)], 4, 10);
        for req in &b {
            assert!(req.iter().map(|i| i.len).sum::<u64>() <= 4);
        }
        assert_eq!(b.iter().flatten().map(|i| i.len).sum::<u64>(), 15);
        let bs: Vec<_> = b.iter().flatten().filter(|i| i.sha == "b").map(|i| (i.offset, i.len)).collect();
        assert_eq!(bs, vec![(2, 1), (3, 4), (7, 4), (11, 1)]);
        // Item cap.
        let many: Vec<_> = (0..5).map(|i| r(&i.to_string(), 0, 1)).collect();
        assert_eq!(pack_of(&many, 100, 2).iter().map(Vec::len).collect::<Vec<_>>(), vec![2, 2, 1]);
    }

    #[test]
    fn resume_reuses_a_prefix_and_restarts_a_longer_file() {
        let t = crate::sync::files::tests::tempdir();
        let p = t.path().join("x.part");
        std::fs::write(&p, b"abc").unwrap();
        let mut part = resume(&p, 6).unwrap();
        assert_eq!(part.written, 3);
        part.file.write_all(b"def").unwrap();
        part.hasher.update(b"def");
        part.written = 6;
        let sha = hex::encode(Sha256::digest(b"abcdef"));
        let part_path = t.path().join(format!("{sha}.part"));
        std::fs::rename(&p, &part_path).unwrap();
        let done = finish(t.path(), t.path(), &sha, part).unwrap();
        assert_eq!(std::fs::read(done).unwrap(), b"abcdef");
        std::fs::write(&p, b"too long").unwrap();
        assert_eq!(resume(&p, 2).unwrap().written, 0);
        assert_eq!(std::fs::metadata(&p).unwrap().len(), 0);
    }
}
