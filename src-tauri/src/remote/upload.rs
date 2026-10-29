//! Remote desktop uploads: files the controlling Devdy (A) drops or pastes into
//! a remote-desktop window are copied onto this machine (B) before the UI uses
//! them, because every path the UI hands to a command must be a path on B.
//!
//! Two halves, both plain Tauri commands:
//!   - B side (reached through the IPC bridge like any other command):
//!     `remote_upload_begin` → `remote_upload_chunk`… → `remote_upload_finish`
//!     (or `remote_upload_abort`). Each upload lands in its own directory
//!     `<app cache>/remote-uploads/<upload_id>/<sanitized name>`, so the original
//!     file name survives and two drops of `a.png` never collide. Directories
//!     older than [`KEEP_UPLOADS`] are pruned on the next upload.
//!   - A side (called with the REAL local invoke, see `src/lib/remoteUpload.ts`):
//!     `stat_local_files` / `read_local_file_chunk` read the dropped files.
//!
//! Bytes travel as base64 in JSON, at most [`MAX_CHUNK_BYTES`] per call, and are
//! written strictly in order (each chunk's `offset` must equal what was written).

use base64::Engine;
use serde::Serialize;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tauri::{AppHandle, Manager};

/// Largest single upload.
pub const MAX_UPLOAD_BYTES: u64 = 1024 * 1024 * 1024;
/// Largest decoded chunk per `remote_upload_chunk` / `read_local_file_chunk`.
pub const MAX_CHUNK_BYTES: usize = 4 * 1024 * 1024;
/// Uploads that may be in progress at once.
pub const MAX_OPEN_UPLOADS: usize = 32;
/// An unfinished upload untouched for this long is dropped (and its file removed).
const STALE_UPLOAD: Duration = Duration::from_secs(10 * 60);
/// Finished uploads are kept this long: the agent reads them after the prompt.
const KEEP_UPLOADS: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const MAX_NAME_CHARS: usize = 120;
const UPLOADS_DIR: &str = "remote-uploads";

struct Upload {
    dir: PathBuf,
    path: PathBuf,
    size: u64,
    written: u64,
    touched: Instant,
}

/// Uploads in progress, keyed by upload id.
#[derive(Default)]
pub struct Uploads {
    map: HashMap<String, Upload>,
}

static UPLOADS: LazyLock<Mutex<Uploads>> = LazyLock::new(|| Mutex::new(Uploads::default()));

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UploadBegin {
    pub upload_id: String,
    pub path: String,
}

/// A file name that is safe to create inside the upload directory: the last
/// path component only, no control or separator characters, no leading dots,
/// bounded length (the extension is kept when trimming).
pub fn sanitize_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last
        .chars()
        .map(|c| if c.is_control() || matches!(c, ':' | '/' | '\\') { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim();
    if cleaned.is_empty() {
        return "upload".to_string();
    }
    if cleaned.chars().count() <= MAX_NAME_CHARS {
        return cleaned.to_string();
    }
    let (stem, ext) = match cleaned.rfind('.') {
        Some(i) if i > 0 && cleaned.len() - i <= 16 => (&cleaned[..i], &cleaned[i..]),
        _ => (cleaned, ""),
    };
    let keep = MAX_NAME_CHARS - ext.chars().count();
    format!("{}{}", stem.chars().take(keep).collect::<String>(), ext)
}

impl Uploads {
    /// Start an upload of `size` bytes named `name` under `root`.
    pub fn begin(&mut self, root: &Path, name: &str, size: u64) -> Result<UploadBegin, String> {
        if size > MAX_UPLOAD_BYTES {
            return Err(format!("File too large (max {} MiB)", MAX_UPLOAD_BYTES / 1024 / 1024));
        }
        self.drop_stale(STALE_UPLOAD);
        if self.map.len() >= MAX_OPEN_UPLOADS {
            return Err("Too many uploads in progress".to_string());
        }
        prune_old(root, KEEP_UPLOADS);
        let id = uuid::Uuid::new_v4().simple().to_string();
        let dir = root.join(&id);
        fs::create_dir_all(&dir).map_err(|e| format!("create upload dir: {e}"))?;
        let path = dir.join(sanitize_name(name));
        if let Err(e) = OpenOptions::new().write(true).create_new(true).open(&path) {
            let _ = fs::remove_dir_all(&dir);
            return Err(format!("create upload file: {e}"));
        }
        let shown = path.to_string_lossy().into_owned();
        self.map.insert(
            id.clone(),
            Upload { dir, path, size, written: 0, touched: Instant::now() },
        );
        Ok(UploadBegin { upload_id: id, path: shown })
    }

    /// Append `bytes` at `offset` (must equal the bytes written so far). A failed
    /// write aborts the upload.
    pub fn chunk(&mut self, id: &str, offset: u64, bytes: &[u8]) -> Result<u64, String> {
        let up = self.map.get_mut(id).ok_or_else(|| "Unknown upload".to_string())?;
        if bytes.len() > MAX_CHUNK_BYTES {
            return Err("Chunk too large".to_string());
        }
        if offset != up.written {
            return Err(format!("Unexpected offset {offset} (expected {})", up.written));
        }
        if up.written + bytes.len() as u64 > up.size {
            return Err("Upload exceeds its declared size".to_string());
        }
        let res = OpenOptions::new()
            .append(true)
            .open(&up.path)
            .and_then(|mut f| f.write_all(bytes));
        match res {
            Ok(()) => {
                up.written += bytes.len() as u64;
                up.touched = Instant::now();
                Ok(up.written)
            }
            Err(e) => {
                self.abort(id);
                Err(format!("write upload: {e}"))
            }
        }
    }

    /// Complete an upload; returns its path on this machine.
    pub fn finish(&mut self, id: &str) -> Result<String, String> {
        let up = self.map.get(id).ok_or_else(|| "Unknown upload".to_string())?;
        if up.written != up.size {
            return Err(format!("Upload incomplete ({} of {} bytes)", up.written, up.size));
        }
        let up = self.map.remove(id).expect("checked above");
        Ok(up.path.to_string_lossy().into_owned())
    }

    /// Drop an upload and its partial file (no-op when unknown).
    pub fn abort(&mut self, id: &str) {
        if let Some(up) = self.map.remove(id) {
            let _ = fs::remove_dir_all(&up.dir);
        }
    }

    fn drop_stale(&mut self, max_idle: Duration) {
        let stale: Vec<String> = self
            .map
            .iter()
            .filter(|(_, u)| u.touched.elapsed() >= max_idle)
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            self.abort(&id);
        }
    }
}

/// Remove upload directories under `root` last modified more than `max_age` ago.
fn prune_old(root: &Path, max_age: Duration) {
    let Ok(entries) = fs::read_dir(root) else { return };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age >= max_age);
        if old && entry.path().is_dir() {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

fn uploads_root(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_cache_dir().map_err(|e| e.to_string())?;
    Ok(dir.join(UPLOADS_DIR))
}

fn lock() -> std::sync::MutexGuard<'static, Uploads> {
    UPLOADS.lock().unwrap_or_else(|e| e.into_inner())
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f).await.map_err(|e| format!("join error: {e}"))?
}

#[tauri::command]
pub async fn remote_upload_begin(app: AppHandle, name: String, size: u64) -> Result<UploadBegin, String> {
    let root = uploads_root(&app)?;
    blocking(move || lock().begin(&root, &name, size)).await
}

#[tauri::command]
pub async fn remote_upload_chunk(upload_id: String, offset: u64, data_base64: String) -> Result<u64, String> {
    blocking(move || {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data_base64.as_bytes())
            .map_err(|e| format!("bad chunk: {e}"))?;
        lock().chunk(&upload_id, offset, &bytes)
    })
    .await
}

#[tauri::command]
pub async fn remote_upload_finish(upload_id: String) -> Result<String, String> {
    blocking(move || lock().finish(&upload_id)).await
}

#[tauri::command]
pub async fn remote_upload_abort(upload_id: String) -> Result<(), String> {
    blocking(move || {
        lock().abort(&upload_id);
        Ok(())
    })
    .await
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalFileStat {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    /// Set when the path can't be read (the other fields are then empty).
    pub error: Option<String>,
}

fn stat_one(path: String) -> LocalFileStat {
    let name = Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.clone());
    match fs::metadata(&path) {
        Ok(m) => LocalFileStat { path, name, size: m.len(), is_dir: m.is_dir(), error: None },
        Err(e) => LocalFileStat { path, name, size: 0, is_dir: false, error: Some(e.to_string()) },
    }
}

/// Size / kind of files on THIS machine (the controlling side of a remote
/// desktop reads what the user dropped before uploading it).
#[tauri::command]
pub async fn stat_local_files(paths: Vec<String>) -> Result<Vec<LocalFileStat>, String> {
    blocking(move || Ok(paths.into_iter().map(stat_one).collect())).await
}

fn read_chunk(path: &str, offset: u64, len: usize) -> Result<Vec<u8>, String> {
    let len = len.min(MAX_CHUNK_BYTES);
    let mut f = fs::File::open(path).map_err(|e| format!("open {path}: {e}"))?;
    f.seek(SeekFrom::Start(offset)).map_err(|e| format!("seek {path}: {e}"))?;
    let mut buf = Vec::with_capacity(len);
    f.take(len as u64).read_to_end(&mut buf).map_err(|e| format!("read {path}: {e}"))?;
    Ok(buf)
}

/// Up to `len` bytes (capped at [`MAX_CHUNK_BYTES`]) of a file on THIS machine
/// from `offset`, base64-encoded. Short (or empty) at end of file.
#[tauri::command]
pub async fn read_local_file_chunk(path: String, offset: u64, len: usize) -> Result<String, String> {
    blocking(move || {
        let bytes = read_chunk(&path, offset, len)?;
        Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("devdy-upload-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sanitize_keeps_only_a_safe_file_name() {
        assert_eq!(sanitize_name("photo.png"), "photo.png");
        assert_eq!(sanitize_name("/Users/me/../../etc/passwd"), "passwd");
        assert_eq!(sanitize_name("C:\\dir\\a.txt"), "a.txt");
        assert_eq!(sanitize_name(".."), "upload");
        assert_eq!(sanitize_name(".env"), "env");
        assert_eq!(sanitize_name(""), "upload");
        assert_eq!(sanitize_name("a\u{0}b:c"), "a_b_c");
        let long = format!("{}.tar.gz", "x".repeat(300));
        let s = sanitize_name(&long);
        assert_eq!(s.chars().count(), MAX_NAME_CHARS);
        assert!(s.ends_with(".gz"));
    }

    #[test]
    fn upload_round_trip_writes_the_file_in_order() {
        let root = tmp_root();
        let mut ups = Uploads::default();
        let b = ups.begin(&root, "../notes.txt", 6).unwrap();
        assert!(b.path.ends_with("notes.txt"));
        assert!(Path::new(&b.path).starts_with(&root));
        assert_eq!(ups.chunk(&b.upload_id, 0, b"abc").unwrap(), 3);
        // Out of order / replayed chunks are refused.
        assert!(ups.chunk(&b.upload_id, 0, b"abc").is_err());
        assert!(ups.finish(&b.upload_id).is_err(), "incomplete");
        assert_eq!(ups.chunk(&b.upload_id, 3, b"def").unwrap(), 6);
        let path = ups.finish(&b.upload_id).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"abcdef");
        assert!(ups.finish(&b.upload_id).is_err(), "already finished");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn limits_are_enforced() {
        let root = tmp_root();
        let mut ups = Uploads::default();
        assert!(ups.begin(&root, "big", MAX_UPLOAD_BYTES + 1).is_err());
        let b = ups.begin(&root, "a", 2).unwrap();
        assert!(ups.chunk(&b.upload_id, 0, b"abc").is_err(), "over declared size");
        assert!(ups.chunk("nope", 0, b"a").is_err());
        let dir = Path::new(&b.path).parent().unwrap().to_path_buf();
        ups.abort(&b.upload_id);
        assert!(!dir.exists(), "abort removes the partial file");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_uploads_are_dropped() {
        let root = tmp_root();
        let mut ups = Uploads::default();
        let b = ups.begin(&root, "a", 1).unwrap();
        ups.drop_stale(Duration::ZERO);
        assert!(ups.chunk(&b.upload_id, 0, b"a").is_err());
        assert!(!Path::new(&b.path).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_chunks_read_ranges() {
        let root = tmp_root();
        let p = root.join("f.bin");
        fs::write(&p, b"0123456789").unwrap();
        let p = p.to_string_lossy().into_owned();
        assert_eq!(read_chunk(&p, 2, 3).unwrap(), b"234");
        assert_eq!(read_chunk(&p, 8, 10).unwrap(), b"89");
        assert!(read_chunk(&p, 20, 10).unwrap().is_empty());
        let st = stat_one(p.clone());
        assert_eq!((st.size, st.is_dir, st.name.as_str()), (10, false, "f.bin"));
        assert!(stat_one(root.join("missing").to_string_lossy().into_owned()).error.is_some());
        fs::remove_dir_all(root).unwrap();
    }
}
